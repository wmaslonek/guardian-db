//! User-defined functions (`CREATE FUNCTION` / `DROP FUNCTION`).
//!
//! Two languages are supported, matching what PostgreSQL calls "SQL-language"
//! and "PL/pgSQL" functions:
//!
//! * `LANGUAGE SQL` — the body is one or more `;`-separated plain SQL
//!   statements (`SELECT`/`INSERT`/`UPDATE`/`DELETE`); arguments are bound
//!   both positionally (`$1`, `$2`, ... exactly like a prepared statement)
//!   and by the declared parameter name, matching PostgreSQL (a SQL-language
//!   function may reference its arguments either way). All statements
//!   execute in order; the last statement's result becomes the function's
//!   result (its first row's first column, or `NULL` if it produced no
//!   rows) — this matches PostgreSQL.
//! * `LANGUAGE plpgsql` — a small, explicitly-bounded subset: `DECLARE`d
//!   locals with optional defaults, `:=` assignment, `IF`/`ELSIF`/`ELSE`/
//!   `END IF`, `RETURN [expr]`, `RAISE [NOTICE|WARNING|EXCEPTION] 'msg'[,
//!   args]`, and plain SQL statements. Arguments and locals are referenced by
//!   name (not `$n`). Anything outside this subset (loops, `EXCEPTION`
//!   handlers, cursors, dynamic `EXECUTE`, nested blocks, `OUT`/`INOUT`/
//!   `VARIADIC` parameters, `RETURNS TABLE`/`SETOF`) is rejected with a typed
//!   `0A000` naming the construct — see `docs/postgres-compat.md`.
//!
//! sqlparser has no PL/pgSQL grammar (a function body is opaque `$$...$$`
//! text handed to the language handler, exactly as in real PostgreSQL), so
//! [`parse_plpgsql`] is a small hand-written recursive-descent parser built
//! on top of sqlparser's tokenizer/expression parser (reused for every
//! embedded SQL statement and every expression, so `IF n > 0 THEN` and
//! `x := a + b` parse and evaluate with the exact same grammar as top-level
//! SQL).
//!
//! **Deliberate divergence from PostgreSQL**: real PostgreSQL does not
//! validate a PL/pgSQL body's structure at `CREATE FUNCTION` time (without
//! the separate `plpgsql_check` extension) — a function with a typo or an
//! unsupported construct is only discovered when it is first called. This
//! repo's truthfulness contract requires every construct to either work or
//! fail typed *immediately*, so unsupported constructs and structural
//! problems (e.g. control reaching the end of the function without a
//! `RETURN`) are rejected at `CREATE FUNCTION` (DDL) time instead — a broken
//! function can never silently exist in the catalog.
//!
//! Bodies are stored as raw source text (`prosrc`) and re-parsed on every
//! call, the same pattern row-security policy expressions already follow
//! (see [`crate::sql::rls`]).

use crate::relational::catalog::{
    DropFunctionByName, FunctionArgDef, FunctionDef, FunctionLanguage, FunctionVolatility, Table,
    TriggerLevel, TriggerTiming,
};
use crate::relational::{Catalog, SqlType, SqlValue};
use crate::sql::error::{Result, SqlError, unsupported};
use crate::sql::exec::Exec;
use crate::sql::names::{ident_name, split_schema_table};
use crate::sql::result::ExecResult;
use crate::sql::store::RowValues;
use sqlparser::ast::{
    ArgMode, CreateFunction, CreateFunctionBody, DataType, DropFunction, Expr, Function,
    FunctionArg, FunctionArgExpr, FunctionArguments, FunctionBehavior, FunctionCalledOnNull,
    FunctionReturnType, Ident, OnConflictAction, OnInsert, OperateFunctionArg, Query, Select,
    SelectItem, SetExpr, Statement, TableWithJoins, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Token;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering;

/// The bounded recursion budget for user-defined-function calls, shared
/// across an entire statement's call chain (see `Exec::udf_depth`) so
/// self-recursion is bounded regardless of Rust call-stack depth.
/// PostgreSQL's analogous guard is a stack-depth check reported as SQLSTATE
/// 54001 (`statement_too_complex`); reused here for the same reason (no hang,
/// no unbounded native stack growth).
///
/// Each nested call re-parses the callee's body and recurses through the
/// expression evaluator/statement executor on the real Rust call stack (see
/// [`call_function_inner`]), so — unlike the `WITH RECURSIVE` iteration cap,
/// which bounds a `loop`, not stack depth — this constant is sized to stay
/// well under a worker thread's stack (e.g. Tokio's 2 MiB default) rather
/// than to match PostgreSQL's own default `max_stack_depth`.
const MAX_CALL_DEPTH: u32 = 25;

// ---------------------------------------------------------------------------
// CREATE / DROP FUNCTION
// ---------------------------------------------------------------------------

impl Exec {
    pub fn exec_create_function(&mut self, cf: &CreateFunction) -> Result<ExecResult> {
        if cf.temporary {
            return Err(unsupported("CREATE TEMPORARY FUNCTION"));
        }
        let (schema_opt, name) = split_schema_table(&cf.name);
        let schema = self.catalog.creation_schema(schema_opt.as_deref())?;
        let args = parse_function_args(cf.args.as_deref().unwrap_or(&[]))?;
        let (return_type, returns_trigger) = parse_return_type(&cf.return_type, &name)?;
        let language = parse_language(cf.language.as_ref())?;
        if returns_trigger {
            // PostgreSQL's own rules (both are SQLSTATE 42P13 there too).
            if language == FunctionLanguage::Sql {
                return Err(SqlError::InvalidFunctionDefinition(
                    "SQL functions cannot return type trigger".into(),
                ));
            }
            if !args.is_empty() {
                return Err(SqlError::InvalidFunctionDefinition(
                    "trigger functions cannot have declared arguments".into(),
                ));
            }
        }
        let volatility = match cf.behavior {
            Some(FunctionBehavior::Immutable) => FunctionVolatility::Immutable,
            Some(FunctionBehavior::Stable) => FunctionVolatility::Stable,
            Some(FunctionBehavior::Volatile) | None => FunctionVolatility::Volatile,
        };
        let strict = matches!(
            cf.called_on_null,
            Some(FunctionCalledOnNull::Strict) | Some(FunctionCalledOnNull::ReturnsNullOnNullInput)
        );
        let body = extract_body_text(cf.function_body.as_ref(), &name)?;

        // Reject at DDL time (see module docs): syntax plus the fixed
        // unsupported-construct list. Table/column existence is still
        // deferred to call time, matching PostgreSQL (a function may
        // legitimately reference a table created later).
        match language {
            FunctionLanguage::Sql => {
                parse_body_statements(&body)?;
            }
            FunctionLanguage::PlPgSql => {
                let prog = parse_plpgsql(&body)?;
                if !always_returns(&prog.body) {
                    return Err(SqlError::InvalidFunctionDefinition(format!(
                        "function \"{name}\" control reached end of function without RETURN"
                    )));
                }
            }
        }

        // `CREATE OR REPLACE` may not turn a trigger function that live
        // triggers depend on into a non-trigger function — the triggers would
        // dangle (PostgreSQL rejects changing the return type similarly).
        if cf.or_replace
            && !returns_trigger
            && let Some(existing) = self.catalog.find_function(Some(&schema), &name, args.len())
            && existing.returns_trigger
            && let Some((trg, tbl)) = trigger_dependent(&self.catalog, &schema, &name)
        {
            return Err(SqlError::DependentObjectsStillExist {
                object: format!("function {name}"),
                detail: format!(
                    "trigger {trg} on table {tbl} depends on function {name}; the \
                     replacement no longer returns type trigger"
                ),
            });
        }

        let oid = self.catalog.allocate_oid();
        let def = FunctionDef {
            oid,
            schema,
            name: name.clone(),
            args,
            return_type,
            language,
            volatility,
            strict,
            body,
            returns_trigger,
        };
        if cf.or_replace {
            self.catalog.replace_function(def);
        } else {
            self.catalog.insert_function(def)?;
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("CREATE FUNCTION"))
    }

    pub fn exec_drop_function(&mut self, df: &DropFunction) -> Result<ExecResult> {
        for desc in &df.func_desc {
            let (schema_opt, name) = split_schema_table(&desc.name);
            match &desc.args {
                Some(arg_list) => {
                    // Dependency guard: a trigger function still wired to a
                    // trigger cannot be dropped (PostgreSQL: 2BP01).
                    let dependent = self
                        .catalog
                        .find_function(schema_opt.as_deref(), &name, arg_list.len())
                        .filter(|def| def.returns_trigger)
                        .and_then(|def| trigger_dependent(&self.catalog, &def.schema, &def.name));
                    if let Some((trg, tbl)) = dependent {
                        return Err(SqlError::DependentObjectsStillExist {
                            object: format!("function {name}"),
                            detail: format!(
                                "trigger {trg} on table {tbl} depends on function {name}"
                            ),
                        });
                    }
                    let removed =
                        self.catalog
                            .drop_function(schema_opt.as_deref(), &name, arg_list.len());
                    if !removed && !df.if_exists {
                        let types = arg_list
                            .iter()
                            .map(|a| a.data_type.to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(SqlError::UndefinedFunction(format!("{name}({types})")));
                    }
                }
                None => {
                    // Same dependency guard for the unqualified form. Only an
                    // unambiguous single match can be dropped anyway.
                    let candidates: Vec<(String, String, bool)> = self
                        .catalog
                        .functions()
                        .filter(|f| {
                            f.name == name
                                && schema_opt.as_deref().map(|s| f.schema == s).unwrap_or(true)
                        })
                        .map(|f| (f.schema.clone(), f.name.clone(), f.returns_trigger))
                        .collect();
                    if let [(fschema, fname, true)] = candidates.as_slice()
                        && let Some((trg, tbl)) = trigger_dependent(&self.catalog, fschema, fname)
                    {
                        return Err(SqlError::DependentObjectsStillExist {
                            object: format!("function {fname}"),
                            detail: format!(
                                "trigger {trg} on table {tbl} depends on function {fname}"
                            ),
                        });
                    }
                    match self
                        .catalog
                        .drop_function_by_name(schema_opt.as_deref(), &name)
                    {
                        DropFunctionByName::Removed => {}
                        DropFunctionByName::NotFound => {
                            if !df.if_exists {
                                return Err(SqlError::UndefinedFunction(name.clone()));
                            }
                        }
                        DropFunctionByName::Ambiguous => {
                            return Err(SqlError::AmbiguousFunction(format!(
                                "function name \"{name}\" is not unique; specify the argument \
                                 list to select the function unambiguously"
                            )));
                        }
                    }
                }
            }
        }
        self.catalog_dirty = true;
        Ok(ExecResult::empty_command("DROP FUNCTION"))
    }
}

fn parse_function_args(args: &[OperateFunctionArg]) -> Result<Vec<FunctionArgDef>> {
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            match a.mode {
                Some(ArgMode::Out) => return Err(unsupported("OUT parameters")),
                Some(ArgMode::InOut) => return Err(unsupported("INOUT parameters")),
                Some(ArgMode::Variadic) => return Err(unsupported("VARIADIC parameters")),
                Some(ArgMode::In) | None => {}
            }
            let name = a
                .name
                .as_ref()
                .map(ident_name)
                .unwrap_or_else(|| format!("${}", i + 1));
            let ty = crate::sql::eval::parse_data_type(&a.data_type)?;
            Ok(FunctionArgDef { name, ty })
        })
        .collect()
}

/// Parse the `RETURNS` clause into `(type, returns_trigger)`. `RETURNS
/// trigger` deliberately maps to `(SqlType::Unknown, true)` instead of a new
/// `SqlType` variant — `SqlType` is pervasive (`parse_data_type`, casts,
/// `pg_type`), and a variant would make `CREATE TABLE t (x trigger)`
/// representable. The caller enforces the trigger-function rules (PL/pgSQL
/// only, zero arguments).
fn parse_return_type(rt: &Option<FunctionReturnType>, fname: &str) -> Result<(SqlType, bool)> {
    match rt {
        None => Err(SqlError::InvalidFunctionDefinition(format!(
            "function \"{fname}\" has no RETURNS clause"
        ))),
        Some(FunctionReturnType::SetOf(_)) => Err(unsupported("RETURNS SETOF")),
        Some(FunctionReturnType::DataType(DataType::Table(_))) => Err(unsupported("RETURNS TABLE")),
        Some(FunctionReturnType::DataType(dt)) => {
            if dt.to_string().eq_ignore_ascii_case("trigger") {
                return Ok((SqlType::Unknown, true));
            }
            Ok((crate::sql::eval::parse_data_type(dt)?, false))
        }
    }
}

/// The first trigger found in `catalog` whose function is `(schema, name)`,
/// as `(trigger name, table name)` — the `DROP FUNCTION` / `CREATE OR
/// REPLACE` dependency guard (trigger functions always have arity 0, so
/// (schema, name) identifies the referenced signature).
fn trigger_dependent(catalog: &Catalog, schema: &str, name: &str) -> Option<(String, String)> {
    for table in catalog.tables() {
        for trg in &table.triggers {
            if trg.function_schema == schema && trg.function_name == name {
                return Some((trg.name.clone(), table.name.clone()));
            }
        }
    }
    None
}

fn parse_language(lang: Option<&Ident>) -> Result<FunctionLanguage> {
    let Some(lang) = lang else {
        return Err(SqlError::InvalidFunctionDefinition(
            "LANGUAGE clause is required".into(),
        ));
    };
    let name = ident_name(lang);
    match name.as_str() {
        "sql" => Ok(FunctionLanguage::Sql),
        "plpgsql" => Ok(FunctionLanguage::PlPgSql),
        "c" | "internal" | "plpython3u" | "plperl" | "plperlu" | "pltcl" => {
            Err(unsupported(format!("LANGUAGE {name}")))
        }
        other => Err(SqlError::UndefinedObject(format!(
            "language \"{other}\" does not exist"
        ))),
    }
}

fn extract_body_text(body: Option<&CreateFunctionBody>, fname: &str) -> Result<String> {
    let (expr, has_link_symbol) = match body {
        None => {
            return Err(SqlError::InvalidFunctionDefinition(format!(
                "function \"{fname}\" has no function body (AS clause)"
            )));
        }
        Some(CreateFunctionBody::AsBeforeOptions { body, link_symbol }) => {
            (body, link_symbol.is_some())
        }
        Some(CreateFunctionBody::AsAfterOptions(body)) => (body, false),
        Some(other) => {
            return Err(unsupported(format!("function body form: {other:?}")));
        }
    };
    if has_link_symbol {
        return Err(unsupported("LANGUAGE C functions"));
    }
    match expr {
        Expr::Value(vws) => match &vws.value {
            Value::DollarQuotedString(d) => Ok(d.value.clone()),
            Value::SingleQuotedString(s) => Ok(s.clone()),
            other => Err(SqlError::Syntax(format!(
                "unsupported function body literal: {other}"
            ))),
        },
        other => Err(SqlError::Syntax(format!(
            "unsupported function body expression: {other}"
        ))),
    }
}

/// Parse a `LANGUAGE SQL` body into its statement list, rejecting anything
/// but plain `SELECT`/`INSERT`/`UPDATE`/`DELETE`.
fn parse_body_statements(body: &str) -> Result<Vec<Statement>> {
    let stmts = crate::sql::parser::parse_sql(body)?;
    if stmts.is_empty() {
        return Err(SqlError::InvalidFunctionDefinition(
            "function body must contain at least one statement".into(),
        ));
    }
    for s in &stmts {
        ensure_supported_body_stmt(s)?;
    }
    Ok(stmts)
}

fn ensure_supported_body_stmt(stmt: &Statement) -> Result<()> {
    match stmt {
        Statement::Query(_)
        | Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_) => Ok(()),
        other => Err(SqlError::FeatureNotSupported(format!(
            "{} is not supported inside a function body",
            stmt_kind_name(other)
        ))),
    }
}

/// A short, human-readable name for a statement kind (`"CREATE TABLE"`,
/// `"ALTER TABLE"`, ...), used only for error messages.
fn stmt_kind_name(stmt: &Statement) -> String {
    stmt.to_string()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// PL/pgSQL: AST + recursive-descent parser
// ---------------------------------------------------------------------------

struct PlpgsqlProgram {
    decls: Vec<PlpgsqlDecl>,
    body: Vec<PlpgsqlStmt>,
}

struct PlpgsqlDecl {
    name: String,
    default: Option<Expr>,
}

/// The target of a PL/pgSQL `:=` assignment: a scalar variable, or a record
/// field (`NEW.col := ...`, trigger functions only).
enum AssignTarget {
    Var(String),
    RowField { row: String, column: String },
}

impl AssignTarget {
    fn display(&self) -> String {
        match self {
            AssignTarget::Var(n) => n.clone(),
            AssignTarget::RowField { row, column } => format!("{row}.{column}"),
        }
    }
}

enum PlpgsqlStmt {
    Assign {
        target: AssignTarget,
        value: Expr,
    },
    If {
        branches: Vec<(Expr, Vec<PlpgsqlStmt>)>,
        else_branch: Option<Vec<PlpgsqlStmt>>,
    },
    Return(Option<Expr>),
    Raise {
        level: RaiseLevel,
        message: String,
        args: Vec<Expr>,
    },
    Sql(Box<Statement>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RaiseLevel {
    Notice,
    Warning,
    Exception,
}

/// Statement-leading keywords that put the body outside the supported
/// PL/pgSQL subset, each with the name used in the resulting `0A000` message.
const UNSUPPORTED_STMT_KEYWORDS: &[(&str, &str)] = &[
    ("FOR", "FOR loop"),
    ("WHILE", "WHILE loop"),
    ("LOOP", "LOOP"),
    ("EXCEPTION", "EXCEPTION handler"),
    ("EXECUTE", "dynamic SQL (EXECUTE)"),
    ("DECLARE", "nested block"),
    ("BEGIN", "nested block"),
    ("PERFORM", "PERFORM"),
    ("OPEN", "cursors"),
    ("FETCH", "cursors"),
    ("CLOSE", "cursors"),
    ("GET", "GET DIAGNOSTICS"),
];

fn parse_plpgsql(body: &str) -> Result<PlpgsqlProgram> {
    let mut p = Parser::new(&PostgreSqlDialect {})
        .try_with_sql(body)
        .map_err(crate::sql::error::parse_error)?;
    let decls = if peek_word(&p).as_deref() == Some("DECLARE") {
        p.next_token();
        parse_decls(&mut p)?
    } else {
        Vec::new()
    };
    expect_word(&mut p, "BEGIN")?;
    let body_stmts = parse_stmt_list(&mut p, &["END"])?;
    expect_word(&mut p, "END")?;
    // Optional trailing block label (`END foo;`); best-effort skip of a
    // single identifier before the terminating `;`.
    if matches!(p.peek_token_ref().token, Token::Word(_)) {
        p.next_token();
    }
    expect_semi(&mut p)?;
    if p.peek_token_ref().token != Token::EOF {
        return Err(unsupported("content after the function body's closing END"));
    }
    Ok(PlpgsqlProgram {
        decls,
        body: body_stmts,
    })
}

fn peek_word(p: &Parser) -> Option<String> {
    match &p.peek_token_ref().token {
        Token::Word(w) => Some(w.value.to_ascii_uppercase()),
        _ => None,
    }
}

fn eat_word(p: &mut Parser, word: &str) -> bool {
    if peek_word(p).as_deref() == Some(word) {
        p.next_token();
        true
    } else {
        false
    }
}

fn expect_word(p: &mut Parser, word: &str) -> Result<()> {
    if eat_word(p, word) {
        Ok(())
    } else {
        Err(SqlError::Syntax(format!(
            "expected {word} in function body, found \"{}\"",
            p.peek_token_ref().token
        )))
    }
}

fn expect_semi(p: &mut Parser) -> Result<()> {
    p.expect_token(&Token::SemiColon)
        .map(|_| ())
        .map_err(crate::sql::error::parse_error)
}

fn parse_decls(p: &mut Parser) -> Result<Vec<PlpgsqlDecl>> {
    let mut out = Vec::new();
    loop {
        if peek_word(p).as_deref() == Some("BEGIN") {
            break;
        }
        if p.peek_token_ref().token == Token::EOF {
            return Err(SqlError::Syntax(
                "unexpected end of input in DECLARE section".into(),
            ));
        }
        let ident = p
            .parse_identifier()
            .map_err(crate::sql::error::parse_error)?;
        let name = ident_name(&ident);
        if peek_word(p).as_deref() == Some("CONSTANT") {
            p.next_token();
        }
        if peek_word(p).as_deref() == Some("CURSOR") {
            return Err(unsupported("cursors"));
        }
        let dt = p
            .parse_data_type()
            .map_err(crate::sql::error::parse_error)?;
        // Type-check the declared type; the value itself is not retained
        // (PL/pgSQL variables are dynamically typed in this simplified
        // interpreter — assignments are not checked against it).
        crate::sql::eval::parse_data_type(&dt)?;
        if peek_word(p).as_deref() == Some("NOT") {
            p.next_token();
            expect_word(p, "NULL")?;
        }
        let default = if p.peek_token_ref().token == Token::Assignment {
            p.next_token();
            Some(p.parse_expr().map_err(crate::sql::error::parse_error)?)
        } else if eat_word(p, "DEFAULT") {
            Some(p.parse_expr().map_err(crate::sql::error::parse_error)?)
        } else {
            None
        };
        expect_semi(p)?;
        out.push(PlpgsqlDecl { name, default });
    }
    Ok(out)
}

fn parse_stmt_list(p: &mut Parser, terminators: &[&str]) -> Result<Vec<PlpgsqlStmt>> {
    let mut out = Vec::new();
    loop {
        if let Some(w) = peek_word(p)
            && terminators.contains(&w.as_str())
        {
            break;
        }
        if p.peek_token_ref().token == Token::EOF {
            return Err(SqlError::Syntax("unexpected end of function body".into()));
        }
        out.push(parse_stmt(p)?);
    }
    Ok(out)
}

fn parse_stmt(p: &mut Parser) -> Result<PlpgsqlStmt> {
    if let Some(w) = peek_word(p) {
        match w.as_str() {
            "RETURN" => return parse_return(p),
            "RAISE" => return parse_raise(p),
            "IF" => return parse_if(p),
            "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH" => return parse_sql_stmt(p),
            _ => {
                if let Some((_, label)) = UNSUPPORTED_STMT_KEYWORDS.iter().find(|(k, _)| *k == w) {
                    return Err(unsupported(*label));
                }
            }
        }
    }
    parse_assign(p)
}

fn parse_return(p: &mut Parser) -> Result<PlpgsqlStmt> {
    p.next_token();
    let value = if p.peek_token_ref().token == Token::SemiColon {
        None
    } else {
        Some(p.parse_expr().map_err(crate::sql::error::parse_error)?)
    };
    expect_semi(p)?;
    Ok(PlpgsqlStmt::Return(value))
}

fn parse_sql_stmt(p: &mut Parser) -> Result<PlpgsqlStmt> {
    let stmt = p
        .parse_statement()
        .map_err(crate::sql::error::parse_error)?;
    expect_semi(p)?;
    ensure_supported_body_stmt(&stmt)?;
    Ok(PlpgsqlStmt::Sql(Box::new(stmt)))
}

fn parse_assign(p: &mut Parser) -> Result<PlpgsqlStmt> {
    let found = p.peek_token_ref().token.clone();
    let ident = p
        .parse_identifier()
        .map_err(|_| SqlError::Syntax(format!("unexpected token in function body: \"{found}\"")))?;
    let name = ident_name(&ident);
    // `NEW.col := ...` (record-field assignment, trigger functions).
    let target = if p.consume_token(&Token::Period) {
        let col = p
            .parse_identifier()
            .map_err(crate::sql::error::parse_error)?;
        AssignTarget::RowField {
            row: name,
            column: ident_name(&col),
        }
    } else {
        AssignTarget::Var(name)
    };
    if p.peek_token_ref().token != Token::Assignment {
        return Err(SqlError::Syntax(format!(
            "expected \":=\" after \"{}\" in function body",
            target.display()
        )));
    }
    p.next_token();
    let value = p.parse_expr().map_err(crate::sql::error::parse_error)?;
    expect_semi(p)?;
    Ok(PlpgsqlStmt::Assign { target, value })
}

fn parse_if(p: &mut Parser) -> Result<PlpgsqlStmt> {
    p.next_token(); // IF
    let cond = p.parse_expr().map_err(crate::sql::error::parse_error)?;
    expect_word(p, "THEN")?;
    let then_body = parse_stmt_list(p, &["ELSIF", "ELSE", "END"])?;
    let mut branches = vec![(cond, then_body)];
    while peek_word(p).as_deref() == Some("ELSIF") {
        p.next_token();
        let c = p.parse_expr().map_err(crate::sql::error::parse_error)?;
        expect_word(p, "THEN")?;
        let b = parse_stmt_list(p, &["ELSIF", "ELSE", "END"])?;
        branches.push((c, b));
    }
    let else_branch = if peek_word(p).as_deref() == Some("ELSE") {
        p.next_token();
        Some(parse_stmt_list(p, &["END"])?)
    } else {
        None
    };
    expect_word(p, "END")?;
    expect_word(p, "IF")?;
    expect_semi(p)?;
    Ok(PlpgsqlStmt::If {
        branches,
        else_branch,
    })
}

fn parse_raise(p: &mut Parser) -> Result<PlpgsqlStmt> {
    p.next_token(); // RAISE
    let level = match peek_word(p).as_deref() {
        Some("NOTICE") => {
            p.next_token();
            RaiseLevel::Notice
        }
        Some("WARNING") => {
            p.next_token();
            RaiseLevel::Warning
        }
        Some("EXCEPTION") => {
            p.next_token();
            RaiseLevel::Exception
        }
        Some("DEBUG") | Some("LOG") | Some("INFO") => {
            p.next_token();
            RaiseLevel::Notice
        }
        _ => RaiseLevel::Exception, // PostgreSQL's default when omitted.
    };
    if p.peek_token_ref().token == Token::SemiColon {
        p.next_token();
        return Ok(PlpgsqlStmt::Raise {
            level,
            message: String::new(),
            args: Vec::new(),
        });
    }
    let message = p
        .parse_literal_string()
        .map_err(crate::sql::error::parse_error)?;
    let mut args = Vec::new();
    while p.consume_token(&Token::Comma) {
        args.push(p.parse_expr().map_err(crate::sql::error::parse_error)?);
    }
    if peek_word(p).as_deref() == Some("USING") {
        return Err(unsupported("RAISE ... USING"));
    }
    expect_semi(p)?;
    Ok(PlpgsqlStmt::Raise {
        level,
        message,
        args,
    })
}

/// Whether `stmts`, executed straight through, is guaranteed to hit a
/// `RETURN` (or a `RAISE EXCEPTION`, which aborts) before falling off the
/// end — a sound and complete check for this subset, since the only control
/// constructs are straight-line statements and `IF`/`ELSIF`/`ELSE` (no loops
/// or exception handlers to reintroduce fallthrough).
fn always_returns(stmts: &[PlpgsqlStmt]) -> bool {
    match stmts.last() {
        Some(PlpgsqlStmt::Return(_)) => true,
        Some(PlpgsqlStmt::Raise {
            level: RaiseLevel::Exception,
            ..
        }) => true,
        Some(PlpgsqlStmt::If {
            branches,
            else_branch,
        }) => {
            else_branch.as_ref().is_some_and(|e| always_returns(e))
                && branches.iter().all(|(_, b)| always_returns(b))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Preload support: table references a function body might touch.
// ---------------------------------------------------------------------------

/// Every plain-SQL statement embedded in `def`'s body (recursing into
/// PL/pgSQL's `IF`/`ELSIF`/`ELSE` branches), for the engine's statement
/// preload pass — a called function's `INSERT`/`UPDATE`/`DELETE` can touch
/// tables the calling statement's own text never mentions, and table
/// loading is synchronous and preload-driven (see `crate::sql::exec`), so
/// those tables must be folded into the preload set before execution
/// starts. Returns an empty list on a parse failure: bodies are already
/// validated at `CREATE FUNCTION` time, so this only defends against a
/// hand-edited/corrupted catalog document, not user error.
pub fn body_statements(def: &FunctionDef) -> Vec<Statement> {
    match def.language {
        FunctionLanguage::Sql => parse_body_statements(&def.body).unwrap_or_default(),
        FunctionLanguage::PlPgSql => parse_plpgsql(&def.body)
            .map(|p| collect_plpgsql_sql_stmts(&p.body))
            .unwrap_or_default(),
    }
}

fn collect_plpgsql_sql_stmts(stmts: &[PlpgsqlStmt]) -> Vec<Statement> {
    let mut out = Vec::new();
    for s in stmts {
        match s {
            PlpgsqlStmt::Sql(stmt) => out.push((**stmt).clone()),
            PlpgsqlStmt::If {
                branches,
                else_branch,
            } => {
                for (_, b) in branches {
                    out.extend(collect_plpgsql_sql_stmts(b));
                }
                if let Some(b) = else_branch {
                    out.extend(collect_plpgsql_sql_stmts(b));
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Calling a user-defined function.
// ---------------------------------------------------------------------------

/// Call `def` with already-evaluated `args`, from `exec` (the caller's
/// execution context — the top-level statement's, or a parent UDF call's).
/// Builds an owned sub-[`Exec`] seeded from `exec`'s current catalog/tables
/// (so the callee sees committed-so-far state, including any writes made
/// earlier in the same outer statement or by an enclosing recursive call),
/// executes the body, and folds any mutations the body made back into the
/// shared, `Rc`-backed mutation list `exec` itself will flush on commit.
pub fn call_function(exec: &Exec, def: &FunctionDef, args: Vec<SqlValue>) -> Result<SqlValue> {
    if args.len() != def.arity() {
        return Err(SqlError::UndefinedFunction(format!(
            "{}({} args)",
            def.name,
            args.len()
        )));
    }
    if def.strict && args.iter().any(SqlValue::is_null) {
        return Ok(SqlValue::Null);
    }
    let depth = exec.udf_depth.fetch_add(1, Ordering::SeqCst) + 1;
    if depth > MAX_CALL_DEPTH {
        exec.udf_depth.fetch_sub(1, Ordering::SeqCst);
        return Err(SqlError::StatementTooComplex(format!(
            "function call depth limit exceeded (max {MAX_CALL_DEPTH}) while calling \"{}\"",
            def.name
        )));
    }
    let result = call_function_inner(exec, def, args);
    exec.udf_depth.fetch_sub(1, Ordering::SeqCst);
    result
}

fn call_function_inner(exec: &Exec, def: &FunctionDef, args: Vec<SqlValue>) -> Result<SqlValue> {
    match def.language {
        FunctionLanguage::Sql => {
            // PostgreSQL SQL-language bodies may reference declared
            // parameters either positionally (`$1`) or by name; both are
            // supported here — `$n` via `Exec::param` (the same mechanism a
            // prepared statement's placeholders use), names via the same
            // substitution PL/pgSQL bodies use.
            let env = PlBindings::scalar(bind_named_args(&def.args, args.clone()));
            let mut sub = new_sub_exec(exec, args);
            let stmts = parse_body_statements(&def.body)?;
            run_sql_body(&mut sub, &stmts, &env)
        }
        FunctionLanguage::PlPgSql => {
            let mut sub = new_sub_exec(exec, Vec::new());
            let prog = parse_plpgsql(&def.body)?;
            let mut env = PlBindings::scalar(bind_named_args(&def.args, args));
            for decl in &prog.decls {
                let value = match &decl.default {
                    Some(expr) => eval_with_env(&sub, expr, &env)?,
                    None => SqlValue::Null,
                };
                env.vars.insert(decl.name.clone(), value);
            }
            match run_plpgsql_body(&mut sub, &prog.body, &mut env)? {
                Flow::Return(v) => Ok(v),
                Flow::ReturnRow(_) => Err(SqlError::Internal(
                    "trigger-record RETURN from a scalar function invocation".into(),
                )),
                Flow::Fallthrough => Err(SqlError::Internal(
                    "PL/pgSQL function fell through without RETURN (should have been \
                     rejected at CREATE FUNCTION time)"
                        .into(),
                )),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Calling a trigger function (`RETURNS trigger`).
// ---------------------------------------------------------------------------

/// The bounded recursion budget for trigger firings, shared across an entire
/// statement's chain of firings (see `Exec::trigger_depth`) — same design and
/// SQLSTATE (54001) as [`MAX_CALL_DEPTH`]: a trigger whose body writes its
/// own table recurses `exec_insert → fire → body INSERT → exec_insert → ...`
/// through sub-`Exec`s that share the counter, so the guard bounds it
/// regardless of Rust stack shape.
const MAX_TRIGGER_DEPTH: u32 = 25;

/// One trigger firing, handed from the firing engine
/// (`crate::sql::trigger`) to the PL/pgSQL runtime.
pub(crate) struct TriggerInvocation<'a> {
    pub trigger_name: &'a str,
    pub table: &'a Table,
    /// `TG_OP`: `"INSERT"` / `"UPDATE"` / `"DELETE"`.
    pub op: &'static str,
    pub timing: TriggerTiming,
    pub level: TriggerLevel,
    /// The pre-image row (`OLD`): UPDATE/DELETE row firings.
    pub old: Option<RowValues>,
    /// The proposed row (`NEW`): INSERT/UPDATE row firings.
    pub new: Option<RowValues>,
}

/// Run a `RETURNS trigger` PL/pgSQL function for one firing. `Ok(None)` means
/// the function returned `NULL` (suppression for BEFORE ROW firings, ignored
/// otherwise); `Ok(Some(row))` is the — possibly modified — row to proceed
/// with (`RETURN NEW` reflects any `NEW.col := ...` mutations).
///
/// Unlike [`call_function`] this takes `&mut Exec`: the sub-context's loaded
/// tables and catalog (sequence advancement) are folded back into the caller
/// after the body runs, so trigger side effects are visible to the rest of
/// the statement and to subsequent firings — PostgreSQL's command-counter
/// semantics. Scalar UDF calls cannot do this (they run under `&Exec`, deep
/// inside expression evaluation).
pub(crate) fn call_trigger_function(
    exec: &mut Exec,
    def: &FunctionDef,
    inv: TriggerInvocation<'_>,
) -> Result<Option<RowValues>> {
    let depth = exec.trigger_depth.fetch_add(1, Ordering::SeqCst) + 1;
    if depth > MAX_TRIGGER_DEPTH {
        exec.trigger_depth.fetch_sub(1, Ordering::SeqCst);
        return Err(SqlError::StatementTooComplex(format!(
            "trigger call depth limit exceeded (max {MAX_TRIGGER_DEPTH}) while firing \"{}\" \
             on \"{}\"",
            inv.trigger_name, inv.table.name
        )));
    }
    let result = call_trigger_inner(exec, def, inv);
    exec.trigger_depth.fetch_sub(1, Ordering::SeqCst);
    result
}

fn call_trigger_inner(
    exec: &mut Exec,
    def: &FunctionDef,
    inv: TriggerInvocation<'_>,
) -> Result<Option<RowValues>> {
    let mut sub = new_sub_exec(exec, Vec::new());
    let prog = parse_plpgsql(&def.body)?;
    let mut env = PlBindings {
        vars: HashMap::new(),
        rows: HashMap::new(),
        table: Some(inv.table.clone()),
        trigger: true,
    };
    for (name, value) in [
        ("tg_op", inv.op.to_string()),
        ("tg_name", inv.trigger_name.to_string()),
        ("tg_table_name", inv.table.name.clone()),
        ("tg_table_schema", inv.table.schema.clone()),
        ("tg_when", inv.timing.as_sql().to_string()),
        ("tg_level", inv.level.as_sql().to_string()),
    ] {
        env.vars.insert(name.to_string(), SqlValue::Text(value));
    }
    if let Some(old) = inv.old {
        env.rows.insert("old".to_string(), old);
    }
    if let Some(new) = inv.new {
        env.rows.insert("new".to_string(), new);
    }
    for decl in &prog.decls {
        let value = match &decl.default {
            Some(expr) => eval_with_env(&sub, expr, &env)?,
            None => SqlValue::Null,
        };
        env.vars.insert(decl.name.clone(), value);
    }
    let flow = run_plpgsql_body(&mut sub, &prog.body, &mut env)?;
    // Fold the sub-context's state back into the caller: rows the body wrote
    // and sequences it advanced must be visible to the rest of the statement
    // and to later firings (two AFTER INSERT firings appending to an audit
    // table with a serial key must not both draw the same sequence value),
    // and row locks the body queued must actually be acquired.
    exec.tables = std::mem::take(&mut sub.tables);
    exec.catalog_dirty |= sub.catalog_dirty;
    exec.pending_locks
        .borrow_mut()
        .extend(sub.pending_locks.into_inner());
    exec.catalog = sub.catalog;
    match flow {
        Flow::ReturnRow(row) => Ok(row),
        Flow::Return(_) | Flow::Fallthrough => Err(SqlError::Internal(
            "trigger function did not produce a trigger RETURN (should have been rejected \
             at CREATE FUNCTION time)"
                .into(),
        )),
    }
}

/// A fresh, owned execution context for one UDF invocation: a snapshot of
/// the caller's catalog/tables (so the body reads consistent state) plus
/// shared handles for anything that must be visible to the caller
/// afterwards (mutations, the recursion-depth counter).
fn new_sub_exec(exec: &Exec, params: Vec<SqlValue>) -> Exec {
    let mut sub = Exec::new(
        exec.catalog.clone(),
        exec.tables.clone(),
        params,
        exec.now,
        exec.database.clone(),
        exec.username.clone(),
        exec.locks.clone(),
        exec.session_id,
    );
    sub.vars = RefCell::new(exec.vars.borrow().clone());
    sub.mutations = exec.mutations.clone();
    sub.udf_depth = exec.udf_depth.clone();
    sub.trigger_depth = exec.trigger_depth.clone();
    // UDF/trigger bodies can run vector top-k scans too.
    #[cfg(feature = "vector-index")]
    {
        sub.ann = exec.ann.clone();
    }
    // Copy CTE entries so trigger bodies can access transition tables
    // (REFERENCING NEW TABLE / OLD TABLE) injected by the firing engine.
    sub.cte = exec.cte.clone();
    sub
}

/// Bind declared parameter names to call-site argument values, in order.
fn bind_named_args(arg_defs: &[FunctionArgDef], args: Vec<SqlValue>) -> HashMap<String, SqlValue> {
    arg_defs.iter().map(|a| a.name.clone()).zip(args).collect()
}

fn run_sql_body(sub: &mut Exec, stmts: &[Statement], env: &PlBindings) -> Result<SqlValue> {
    let mut last = SqlValue::Null;
    for stmt in stmts {
        let substituted = substitute_stmt(stmt, env);
        last = exec_body_statement(sub, &substituted)?;
    }
    Ok(last)
}

/// Run one embedded plain-SQL statement and reduce its result to the
/// function-body scalar convention: the first row's first column (`NULL`
/// if it produced no rows), or `NULL` for a statement with no result rows
/// at all (e.g. an `INSERT` without `RETURNING`).
fn exec_body_statement(sub: &mut Exec, stmt: &Statement) -> Result<SqlValue> {
    sub.init_rls(stmt)?;
    match stmt {
        Statement::Query(q) => {
            if let Some(with) = &q.with {
                sub.materialize_with(with)?;
            }
            let rs = sub.exec_select_query(q, &[])?;
            Ok(scalar_of_rows(&rs.rows))
        }
        Statement::Insert(insert) => Ok(scalar_of_result(&sub.exec_insert(insert)?)),
        Statement::Update(update) => Ok(scalar_of_result(&sub.exec_update(update)?)),
        Statement::Delete(delete) => Ok(scalar_of_result(&sub.exec_delete(delete)?)),
        other => Err(SqlError::FeatureNotSupported(format!(
            "{} is not supported inside a function body",
            stmt_kind_name(other)
        ))),
    }
}

fn scalar_of_rows(rows: &[Vec<SqlValue>]) -> SqlValue {
    rows.first()
        .and_then(|r| r.first())
        .cloned()
        .unwrap_or(SqlValue::Null)
}

fn scalar_of_result(result: &ExecResult) -> SqlValue {
    match result {
        ExecResult::Rows { rows, .. } => scalar_of_rows(rows),
        ExecResult::Command { .. } => SqlValue::Null,
    }
}

// ---------------------------------------------------------------------------
// PL/pgSQL interpreter.
// ---------------------------------------------------------------------------

/// Variable and record bindings for one function invocation. Scalar function
/// calls carry an empty `rows` map, `table: None` and `trigger: false` —
/// every trigger-only code path below is gated on those, so plain-UDF
/// behavior is identical to what it was before triggers existed.
struct PlBindings {
    /// Named scalars: parameters, `DECLARE`d locals, and — for trigger
    /// invocations — the pre-seeded `TG_*` variables.
    vars: HashMap<String, SqlValue>,
    /// Trigger records (`"new"` / `"old"`), where bound for this invocation.
    rows: HashMap<String, RowValues>,
    /// The trigger's table, for coercing `NEW.col := ...` assignments.
    table: Option<Table>,
    /// Whether this is a trigger invocation (`RETURN` produces a record).
    trigger: bool,
}

impl PlBindings {
    fn scalar(vars: HashMap<String, SqlValue>) -> Self {
        Self {
            vars,
            rows: HashMap::new(),
            table: None,
            trigger: false,
        }
    }
}

/// Control-flow outcome of running a PL/pgSQL statement list.
enum Flow {
    Fallthrough,
    /// `RETURN expr` of a scalar invocation.
    Return(SqlValue),
    /// `RETURN [NEW | OLD | NULL]` of a trigger invocation: the row to
    /// proceed with, or `None` (`RETURN NULL` / bare `RETURN`) — suppression
    /// for BEFORE ROW firings.
    ReturnRow(Option<RowValues>),
}

fn run_plpgsql_body(sub: &mut Exec, stmts: &[PlpgsqlStmt], env: &mut PlBindings) -> Result<Flow> {
    for stmt in stmts {
        match run_plpgsql_stmt(sub, stmt, env)? {
            Flow::Fallthrough => continue,
            ret => return Ok(ret),
        }
    }
    Ok(Flow::Fallthrough)
}

fn run_plpgsql_stmt(sub: &mut Exec, stmt: &PlpgsqlStmt, env: &mut PlBindings) -> Result<Flow> {
    match stmt {
        PlpgsqlStmt::Assign { target, value } => {
            let v = eval_with_env(sub, value, env)?;
            match target {
                AssignTarget::Var(name) => {
                    env.vars.insert(name.clone(), v);
                }
                AssignTarget::RowField { row, column } => assign_row_field(env, row, column, v)?,
            }
            Ok(Flow::Fallthrough)
        }
        PlpgsqlStmt::Return(expr) => {
            if env.trigger {
                return trigger_return(expr.as_ref(), env);
            }
            let v = match expr {
                Some(e) => eval_with_env(sub, e, env)?,
                None => SqlValue::Null,
            };
            Ok(Flow::Return(v))
        }
        PlpgsqlStmt::Raise {
            level,
            message,
            args,
        } => {
            let text = render_raise_message(sub, message, args, env)?;
            match level {
                // NOTICE/WARNING are accepted and are no-ops: GuardianDB has
                // no client notice channel to surface them on (see
                // `docs/postgres-compat.md`).
                RaiseLevel::Notice | RaiseLevel::Warning => Ok(Flow::Fallthrough),
                RaiseLevel::Exception => Err(SqlError::RaisedException(text)),
            }
        }
        PlpgsqlStmt::If {
            branches,
            else_branch,
        } => {
            for (cond, body) in branches {
                let c = eval_with_env(sub, cond, env)?;
                if c.truthy() == Some(true) {
                    return run_plpgsql_body(sub, body, env);
                }
            }
            match else_branch {
                Some(body) => run_plpgsql_body(sub, body, env),
                None => Ok(Flow::Fallthrough),
            }
        }
        PlpgsqlStmt::Sql(stmt) => {
            let substituted = substitute_stmt(stmt, env);
            exec_body_statement(sub, &substituted)?;
            Ok(Flow::Fallthrough)
        }
    }
}

/// `NEW.col := value` (trigger invocations only): coerce against the
/// trigger's table and update the bound `NEW` record. Everything else fails
/// typed — assigning `OLD` (or any other record) has no effect in this engine
/// and silently accepting it would violate the truthfulness contract.
fn assign_row_field(env: &mut PlBindings, row: &str, column: &str, v: SqlValue) -> Result<()> {
    if !env.trigger {
        return Err(unsupported(format!(
            "assignment to record field \"{row}.{column}\" outside a trigger function"
        )));
    }
    if row != "new" {
        return Err(unsupported(
            "assignment to OLD / non-NEW record fields in a trigger function",
        ));
    }
    let table = env
        .table
        .as_ref()
        .ok_or_else(|| SqlError::Internal("trigger invocation without a table".into()))?;
    let coerced = crate::sql::dml::coerce_to_col(v, table, column)?;
    match env.rows.get_mut("new") {
        Some(new_row) => {
            new_row.insert(column.to_string(), coerced);
            Ok(())
        }
        // DELETE / statement-level firings have no NEW record (PostgreSQL's
        // wording and SQLSTATE).
        None => Err(SqlError::ObjectNotInPrerequisiteState(
            "record \"new\" is not assigned yet".into(),
        )),
    }
}

/// `RETURN ...` inside a trigger invocation. Only `NEW`, `OLD`, `NULL` and
/// bare `RETURN` are meaningful for triggers; the row-ness of `NEW`/`OLD`
/// cannot round-trip through a scalar `SqlValue`, so the *expression* is
/// interpreted here instead of being evaluated.
fn trigger_return(expr: Option<&Expr>, env: &PlBindings) -> Result<Flow> {
    let Some(expr) = expr else {
        return Ok(Flow::ReturnRow(None));
    };
    match expr {
        Expr::Value(vws) if matches!(vws.value, Value::Null) => Ok(Flow::ReturnRow(None)),
        Expr::Identifier(ident) => {
            let name = ident_name(ident);
            match name.as_str() {
                "new" | "old" => match env.rows.get(&name) {
                    Some(row) => Ok(Flow::ReturnRow(Some(row.clone()))),
                    None => Err(SqlError::ObjectNotInPrerequisiteState(format!(
                        "record \"{name}\" is not assigned yet"
                    ))),
                },
                _ => Err(unsupported(
                    "returning arbitrary expressions from trigger functions \
                     (only NEW, OLD or NULL)",
                )),
            }
        }
        _ => Err(unsupported(
            "returning arbitrary expressions from trigger functions (only NEW, OLD or NULL)",
        )),
    }
}

fn eval_with_env(sub: &Exec, expr: &Expr, env: &PlBindings) -> Result<SqlValue> {
    let substituted = substitute_expr(expr, env);
    sub.eval(&substituted, &[])
}

/// Render a `RAISE` message, substituting `%` placeholders with `args`'
/// evaluated text values in order (`%%` is a literal `%`). Extra `%`s or
/// extra args are left as-is rather than erroring — a best-effort
/// simplification of PostgreSQL's stricter arity check.
fn render_raise_message(
    sub: &Exec,
    message: &str,
    args: &[Expr],
    env: &PlBindings,
) -> Result<String> {
    if args.is_empty() {
        return Ok(message.to_string());
    }
    let mut out = String::with_capacity(message.len());
    let mut arg_iter = args.iter();
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            out.push('%');
            continue;
        }
        match arg_iter.next() {
            Some(expr) => {
                let v = eval_with_env(sub, expr, env)?;
                out.push_str(&v.to_text().unwrap_or_default());
            }
            None => out.push('%'),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// PL/pgSQL variable substitution.
//
// PL/pgSQL variables are referenced by bare name directly in expressions;
// GuardianDB's expression evaluator only resolves identifiers against table
// columns (see `crate::sql::eval`). Rather than teaching it about a second,
// PL/pgSQL-only namespace, a declared variable's *current value* is
// substituted as a literal directly into the expression/statement AST
// before it reaches the normal evaluator/executor — so `x := a + 1` and
// `UPDATE t SET v = v + amount WHERE id = target_id` run through the exact
// same code as any other expression or statement.
//
// This always prefers the variable over a same-named column when a body
// statement touches a real table — PostgreSQL's own default
// (`plpgsql.variable_conflict`) is stricter (`error` on ambiguity); this is
// a deliberate simplification, documented in `docs/postgres-compat.md`.
// ---------------------------------------------------------------------------

/// Encode `v` as a literal expression by round-tripping through its text
/// representation and an explicit cast to its own type name — reuses the
/// existing text parser/cast machinery instead of hand-building AST nodes
/// for every `SqlValue` variant.
fn value_to_expr(v: &SqlValue) -> Expr {
    if v.is_null() {
        return Expr::Value(Value::Null.into());
    }
    let text = v.to_text().unwrap_or_default().replace('\'', "''");
    let type_name = v.type_of().name();
    let sql = format!("CAST('{text}' AS {type_name})");
    crate::sql::parser::parse_expr(&sql).unwrap_or(Expr::Value(Value::Null.into()))
}

fn substitute_expr(expr: &Expr, env: &PlBindings) -> Expr {
    if let Expr::Identifier(ident) = expr {
        let name = ident_name(ident);
        if let Some(v) = env.vars.get(&name) {
            return value_to_expr(v);
        }
        return expr.clone();
    }
    if let Expr::CompoundIdentifier(parts) = expr {
        // `NEW.col` / `OLD.col` in a trigger invocation: substitute the bound
        // record's field value as a literal, the same mechanism as scalar
        // variables. A column that is not on the trigger's table is left
        // unsubstituted, so the evaluator fails it with 42703 (`NEW` records
        // always carry every table column). Outside trigger invocations
        // `rows` is empty and every compound identifier passes through
        // untouched, exactly as before.
        if parts.len() == 2 {
            let qualifier = ident_name(&parts[0]);
            if let Some(row) = env.rows.get(&qualifier)
                && let Some(v) = row.get(&ident_name(&parts[1]))
            {
                return value_to_expr(v);
            }
        }
        return expr.clone();
    }
    let mut out = expr.clone();
    match &mut out {
        Expr::BinaryOp { left, right, .. } => {
            **left = substitute_expr(left, env);
            **right = substitute_expr(right, env);
        }
        Expr::UnaryOp { expr: inner, .. }
        | Expr::Nested(inner)
        | Expr::IsNull(inner)
        | Expr::IsNotNull(inner)
        | Expr::IsTrue(inner)
        | Expr::IsNotTrue(inner)
        | Expr::IsFalse(inner)
        | Expr::IsNotFalse(inner)
        | Expr::IsUnknown(inner)
        | Expr::IsNotUnknown(inner) => {
            **inner = substitute_expr(inner, env);
        }
        Expr::Cast { expr: inner, .. } => {
            **inner = substitute_expr(inner, env);
        }
        Expr::Between {
            expr: inner,
            low,
            high,
            ..
        } => {
            **inner = substitute_expr(inner, env);
            **low = substitute_expr(low, env);
            **high = substitute_expr(high, env);
        }
        Expr::InList {
            expr: inner, list, ..
        } => {
            **inner = substitute_expr(inner, env);
            for item in list.iter_mut() {
                *item = substitute_expr(item, env);
            }
        }
        Expr::Like {
            expr: inner,
            pattern,
            ..
        }
        | Expr::ILike {
            expr: inner,
            pattern,
            ..
        } => {
            **inner = substitute_expr(inner, env);
            **pattern = substitute_expr(pattern, env);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(o) = operand {
                **o = substitute_expr(o, env);
            }
            for w in conditions.iter_mut() {
                w.condition = substitute_expr(&w.condition, env);
                w.result = substitute_expr(&w.result, env);
            }
            if let Some(e) = else_result {
                **e = substitute_expr(e, env);
            }
        }
        Expr::Function(func) => substitute_function(func, env),
        Expr::InSubquery { expr: inner, .. } => {
            **inner = substitute_expr(inner, env);
        }
        _ => {}
    }
    out
}

fn substitute_function(func: &mut Function, env: &PlBindings) {
    if let FunctionArguments::List(list) = &mut func.args {
        for arg in list.args.iter_mut() {
            let e = match arg {
                FunctionArg::Named { arg, .. }
                | FunctionArg::ExprNamed { arg, .. }
                | FunctionArg::Unnamed(arg) => arg,
            };
            if let FunctionArgExpr::Expr(e) = e {
                *e = substitute_expr(e, env);
            }
        }
    }
}

fn substitute_stmt(stmt: &Statement, env: &PlBindings) -> Statement {
    let mut out = stmt.clone();
    match &mut out {
        Statement::Query(q) => substitute_query(q, env),
        Statement::Insert(insert) => {
            if let Some(src) = &mut insert.source {
                substitute_query(src, env);
            }
            if let Some(OnInsert::OnConflict(oc)) = &mut insert.on
                && let OnConflictAction::DoUpdate(du) = &mut oc.action
            {
                for a in du.assignments.iter_mut() {
                    a.value = substitute_expr(&a.value, env);
                }
                if let Some(sel) = &mut du.selection {
                    *sel = substitute_expr(sel, env);
                }
            }
        }
        Statement::Update(update) => {
            for a in update.assignments.iter_mut() {
                a.value = substitute_expr(&a.value, env);
            }
            if let Some(sel) = &mut update.selection {
                *sel = substitute_expr(sel, env);
            }
        }
        Statement::Delete(delete) => {
            if let Some(sel) = &mut delete.selection {
                *sel = substitute_expr(sel, env);
            }
        }
        _ => {}
    }
    out
}

fn substitute_query(q: &mut Query, env: &PlBindings) {
    if let Some(with) = &mut q.with {
        for cte in with.cte_tables.iter_mut() {
            substitute_query(&mut cte.query, env);
        }
    }
    substitute_setexpr(&mut q.body, env);
}

fn substitute_setexpr(s: &mut SetExpr, env: &PlBindings) {
    match s {
        SetExpr::Select(sel) => substitute_select(sel, env),
        SetExpr::Query(q) => substitute_query(q, env),
        SetExpr::SetOperation { left, right, .. } => {
            substitute_setexpr(left, env);
            substitute_setexpr(right, env);
        }
        SetExpr::Values(v) => {
            for row in v.rows.iter_mut() {
                for e in row.content.iter_mut() {
                    *e = substitute_expr(e, env);
                }
            }
        }
        _ => {}
    }
}

fn substitute_select(sel: &mut Select, env: &PlBindings) {
    if let Some(w) = &mut sel.selection {
        *w = substitute_expr(w, env);
    }
    if let Some(h) = &mut sel.having {
        *h = substitute_expr(h, env);
    }
    for item in sel.projection.iter_mut() {
        match item {
            SelectItem::UnnamedExpr(e) => *e = substitute_expr(e, env),
            SelectItem::ExprWithAlias { expr, .. } => *expr = substitute_expr(expr, env),
            _ => {}
        }
    }
    for twj in sel.from.iter_mut() {
        substitute_twj(twj, env);
    }
}

fn substitute_twj(twj: &mut TableWithJoins, env: &PlBindings) {
    use sqlparser::ast::{JoinConstraint, JoinOperator};
    for j in twj.joins.iter_mut() {
        if let JoinOperator::Inner(JoinConstraint::On(e))
        | JoinOperator::Left(JoinConstraint::On(e))
        | JoinOperator::Right(JoinConstraint::On(e))
        | JoinOperator::FullOuter(JoinConstraint::On(e)) = &mut j.join_operator
        {
            *e = substitute_expr(e, env);
        }
    }
}
