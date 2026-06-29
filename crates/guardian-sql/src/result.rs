//! Statement results returned by the engine.

use guardian_relational::{SqlType, SqlValue};

/// An output column descriptor.
#[derive(Clone, Debug)]
pub struct OutField {
    pub name: String,
    pub ty: SqlType,
    /// Originating table OID and column attribute number, when known (for
    /// RowDescription). Zero means "not a simple column reference".
    pub table_oid: u32,
    pub column_id: i16,
}

impl OutField {
    pub fn new(name: impl Into<String>, ty: SqlType) -> Self {
        Self { name: name.into(), ty, table_oid: 0, column_id: 0 }
    }
}

/// The result of executing a single SQL statement.
#[derive(Clone, Debug)]
pub enum ExecResult {
    /// A row-producing statement (SELECT, RETURNING, SHOW, ...).
    Rows {
        fields: Vec<OutField>,
        rows: Vec<Vec<SqlValue>>,
    },
    /// A command with a completion tag (INSERT/UPDATE/DELETE/DDL/transaction).
    Command { tag: String },
}

impl ExecResult {
    /// The PostgreSQL command-completion tag.
    pub fn command_tag(&self) -> String {
        match self {
            ExecResult::Command { tag } => tag.clone(),
            ExecResult::Rows { rows, .. } => format!("SELECT {}", rows.len()),
        }
    }

    pub fn empty_command(tag: impl Into<String>) -> Self {
        ExecResult::Command { tag: tag.into() }
    }
}
