//! Virtual `information_schema` and `pg_catalog` tables synthesized from the
//! relational catalog.
//!
//! These are generated on demand whenever a query's FROM references one of them,
//! which is what TypeORM, node-postgres, psql and GUI clients rely on for schema
//! introspection, migrations and `synchronize`.

use crate::error::Result;
use crate::row::{FieldRef, RowSchema, RowSet, Tuple};
use guardian_relational::catalog::Table;
use guardian_relational::{Catalog, SqlType, SqlValue};

const DB_OID: i32 = 16000;

/// Return the rows of a known introspection view, or `None` if `name` is not one.
pub fn view_rows(
    catalog: &Catalog,
    schema: Option<&str>,
    name: &str,
    alias: &str,
) -> Result<Option<RowSet>> {
    // Resolve which catalog the view lives in.
    let in_info = schema == Some("information_schema");
    let in_pg = schema == Some("pg_catalog") || (schema.is_none() && name.starts_with("pg_"));
    let unq = schema.is_none();

    let built = match (in_info || (unq && !name.starts_with("pg_")), name) {
        (true, "tables") => Some(tables(catalog)),
        (true, "columns") => Some(columns(catalog)),
        (true, "schemata") => Some(schemata(catalog)),
        (true, "table_constraints") => Some(table_constraints(catalog)),
        (true, "key_column_usage") => Some(key_column_usage(catalog)),
        (true, "constraint_column_usage") => Some(constraint_column_usage(catalog)),
        (true, "referential_constraints") => Some(referential_constraints(catalog)),
        (true, "views") => Some(views(catalog)),
        _ => None,
    };
    let built = built.or_else(|| match (in_pg, name) {
        (true, "pg_namespace") => Some(pg_namespace(catalog)),
        (true, "pg_class") => Some(pg_class(catalog)),
        (true, "pg_attribute") => Some(pg_attribute(catalog)),
        (true, "pg_type") => Some(pg_type(catalog)),
        (true, "pg_index") => Some(pg_index(catalog)),
        (true, "pg_constraint") => Some(pg_constraint(catalog)),
        (true, "pg_database") => Some(pg_database(catalog)),
        (true, "pg_indexes") => Some(pg_indexes(catalog)),
        (true, "pg_attrdef") => Some(pg_attrdef(catalog)),
        (true, "pg_description") => Some(empty(&[("objoid", SqlType::Integer), ("classoid", SqlType::Integer), ("objsubid", SqlType::Integer), ("description", SqlType::Text)])),
        (true, "pg_enum") => Some(empty(&[("oid", SqlType::Integer), ("enumtypid", SqlType::Integer), ("enumsortorder", SqlType::Real), ("enumlabel", SqlType::Text)])),
        (true, "pg_collation") => Some(empty(&[("oid", SqlType::Integer), ("collname", SqlType::Text), ("collnamespace", SqlType::Integer)])),
        (true, "pg_roles") => Some(pg_roles()),
        (true, "pg_am") => Some(pg_am()),
        (true, "pg_settings") => Some(empty(&[("name", SqlType::Text), ("setting", SqlType::Text)])),
        (true, "pg_inherits") => Some(empty(&[("inhrelid", SqlType::Integer), ("inhparent", SqlType::Integer), ("inhseqno", SqlType::Integer)])),
        _ => None,
    });

    Ok(built.map(|rs| relabel(rs, alias)))
}

// ---------------------------------------------------------------------------
// information_schema
// ---------------------------------------------------------------------------

fn tables(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("table_type", SqlType::Text),
        ("self_referencing_column_name", SqlType::Text),
        ("reference_generation", SqlType::Text),
        ("is_insertable_into", SqlType::Text),
        ("is_typed", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        rows.push(vec![
            t(&catalog.database),
            t(&table.schema),
            t(&table.name),
            t("BASE TABLE"),
            null(),
            null(),
            t("YES"),
            t("NO"),
        ]);
    }
    for v in catalog.views() {
        rows.push(vec![
            t(&catalog.database),
            t(&v.schema),
            t(&v.name),
            t("VIEW"),
            null(),
            null(),
            t("NO"),
            t("NO"),
        ]);
    }
    rs(cols, rows)
}

fn columns(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("column_name", SqlType::Text),
        ("ordinal_position", SqlType::Integer),
        ("column_default", SqlType::Text),
        ("is_nullable", SqlType::Text),
        ("data_type", SqlType::Text),
        ("character_maximum_length", SqlType::Integer),
        ("numeric_precision", SqlType::Integer),
        ("numeric_scale", SqlType::Integer),
        ("datetime_precision", SqlType::Integer),
        ("udt_name", SqlType::Text),
        ("udt_schema", SqlType::Text),
        ("is_identity", SqlType::Text),
        ("is_generated", SqlType::Text),
        ("collation_name", SqlType::Text),
        ("is_updatable", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        for col in &table.columns {
            let (maxlen, prec, scale) = type_metrics(&col.ty);
            rows.push(vec![
                t(&catalog.database),
                t(&table.schema),
                t(&table.name),
                t(&col.name),
                i4(col.ordinal as i32 + 1),
                col.default.clone().map(SqlValue::Text).unwrap_or(null()),
                t(if col.nullable { "YES" } else { "NO" }),
                t(&col.ty.information_schema_name()),
                maxlen,
                prec,
                scale,
                if col.ty.is_temporal() { i4(6) } else { null() },
                t(&col.ty.udt_name()),
                t("pg_catalog"),
                t(if col.identity_sequence.is_some() { "YES" } else { "NO" }),
                t("NEVER"),
                null(),
                t("YES"),
            ]);
        }
    }
    rs(cols, rows)
}

fn schemata(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("catalog_name", SqlType::Text),
        ("schema_name", SqlType::Text),
        ("schema_owner", SqlType::Text),
        ("default_character_set_name", SqlType::Text),
        ("sql_path", SqlType::Text),
    ];
    let rows = catalog
        .schemas()
        .map(|s| vec![t(&catalog.database), t(&s.name), t(&s.owner), null(), null()])
        .collect();
    rs(cols, rows)
}

fn table_constraints(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("constraint_catalog", SqlType::Text),
        ("constraint_schema", SqlType::Text),
        ("constraint_name", SqlType::Text),
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("constraint_type", SqlType::Text),
        ("is_deferrable", SqlType::Text),
        ("initially_deferred", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        if let Some(pk) = &table.primary_key {
            rows.push(constraint_row(catalog, table, &pk.name, "PRIMARY KEY"));
        }
        for u in &table.uniques {
            let name = if u.name.is_empty() { format!("{}_{}_key", table.name, u.columns.join("_")) } else { u.name.clone() };
            rows.push(constraint_row(catalog, table, &name, "UNIQUE"));
        }
        for fk in &table.foreign_keys {
            rows.push(constraint_row(catalog, table, &fk.name, "FOREIGN KEY"));
        }
        for c in &table.checks {
            rows.push(constraint_row(catalog, table, &c.name, "CHECK"));
        }
    }
    rs(cols, rows)
}

fn constraint_row(catalog: &Catalog, table: &Table, name: &str, ctype: &str) -> Tuple {
    vec![
        t(&catalog.database),
        t(&table.schema),
        t(name),
        t(&catalog.database),
        t(&table.schema),
        t(&table.name),
        t(ctype),
        t("NO"),
        t("NO"),
    ]
}

fn key_column_usage(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("constraint_catalog", SqlType::Text),
        ("constraint_schema", SqlType::Text),
        ("constraint_name", SqlType::Text),
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("column_name", SqlType::Text),
        ("ordinal_position", SqlType::Integer),
        ("position_in_unique_constraint", SqlType::Integer),
    ];
    let mut rows = Vec::new();
    let mut push = |table: &Table, name: &str, columns: &[String]| {
        for (i, col) in columns.iter().enumerate() {
            rows.push(vec![
                t(&catalog.database),
                t(&table.schema),
                t(name),
                t(&catalog.database),
                t(&table.schema),
                t(&table.name),
                t(col),
                i4(i as i32 + 1),
                null(),
            ]);
        }
    };
    for table in catalog.tables() {
        if let Some(pk) = &table.primary_key {
            push(table, &pk.name, &pk.columns);
        }
        for u in &table.uniques {
            let name = if u.name.is_empty() { format!("{}_{}_key", table.name, u.columns.join("_")) } else { u.name.clone() };
            push(table, &name, &u.columns);
        }
        for fk in &table.foreign_keys {
            push(table, &fk.name, &fk.columns);
        }
    }
    rs(cols, rows)
}

fn constraint_column_usage(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("column_name", SqlType::Text),
        ("constraint_catalog", SqlType::Text),
        ("constraint_schema", SqlType::Text),
        ("constraint_name", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        for fk in &table.foreign_keys {
            for col in &fk.ref_columns {
                rows.push(vec![
                    t(&catalog.database),
                    t(&fk.ref_schema),
                    t(&fk.ref_table),
                    t(col),
                    t(&catalog.database),
                    t(&table.schema),
                    t(&fk.name),
                ]);
            }
        }
    }
    rs(cols, rows)
}

fn referential_constraints(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("constraint_catalog", SqlType::Text),
        ("constraint_schema", SqlType::Text),
        ("constraint_name", SqlType::Text),
        ("unique_constraint_catalog", SqlType::Text),
        ("unique_constraint_schema", SqlType::Text),
        ("unique_constraint_name", SqlType::Text),
        ("match_option", SqlType::Text),
        ("update_rule", SqlType::Text),
        ("delete_rule", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        for fk in &table.foreign_keys {
            rows.push(vec![
                t(&catalog.database),
                t(&table.schema),
                t(&fk.name),
                t(&catalog.database),
                t(&fk.ref_schema),
                t(&format!("{}_pkey", fk.ref_table)),
                t("NONE"),
                t(fk.on_update.as_sql()),
                t(fk.on_delete.as_sql()),
            ]);
        }
    }
    rs(cols, rows)
}

fn views(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("table_catalog", SqlType::Text),
        ("table_schema", SqlType::Text),
        ("table_name", SqlType::Text),
        ("view_definition", SqlType::Text),
        ("check_option", SqlType::Text),
        ("is_updatable", SqlType::Text),
    ];
    let rows = catalog
        .views()
        .map(|v| vec![t(&catalog.database), t(&v.schema), t(&v.name), t(&v.query), t("NONE"), t("NO")])
        .collect();
    rs(cols, rows)
}

// ---------------------------------------------------------------------------
// pg_catalog
// ---------------------------------------------------------------------------

fn schema_oid(catalog: &Catalog, name: &str) -> i32 {
    catalog.schemas().find(|s| s.name == name).map(|s| s.oid as i32).unwrap_or(0)
}

fn pg_namespace(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("nspname", SqlType::Text),
        ("nspowner", SqlType::Integer),
        ("nspacl", SqlType::Text),
    ];
    let rows = catalog
        .schemas()
        .map(|s| vec![i4(s.oid as i32), t(&s.name), i4(10), null()])
        .collect();
    rs(cols, rows)
}

fn pg_class(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("relname", SqlType::Text),
        ("relnamespace", SqlType::Integer),
        ("reltype", SqlType::Integer),
        ("relowner", SqlType::Integer),
        ("relam", SqlType::Integer),
        ("relkind", SqlType::Char(Some(1))),
        ("relnatts", SqlType::SmallInt),
        ("relhasindex", SqlType::Boolean),
        ("relhaspkey", SqlType::Boolean),
        ("relhasrules", SqlType::Boolean),
        ("relhastriggers", SqlType::Boolean),
        ("relrowsecurity", SqlType::Boolean),
        ("relpersistence", SqlType::Char(Some(1))),
        ("relispartition", SqlType::Boolean),
        ("reltuples", SqlType::Real),
        ("relpages", SqlType::Integer),
        ("relchecks", SqlType::SmallInt),
        ("reltablespace", SqlType::Integer),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        let nidx = catalog.indexes_for_table(&table.schema, &table.name).len();
        rows.push(vec![
            i4(table.oid as i32),
            t(&table.name),
            i4(schema_oid(catalog, &table.schema)),
            i4(0),
            i4(10),
            i4(0),
            t("r"),
            i2(table.columns.len() as i16),
            b(nidx > 0),
            b(table.primary_key.is_some()),
            b(false),
            b(false),
            b(false),
            t("p"),
            b(false),
            SqlValue::Float4(table.columns.len() as f32),
            i4(0),
            i2(table.checks.len() as i16),
            i4(0),
        ]);
    }
    // Indexes also live in pg_class with relkind 'i'.
    for idx in catalog.indexes() {
        rows.push(vec![
            i4(idx.oid as i32),
            t(&idx.name),
            i4(schema_oid(catalog, &idx.schema)),
            i4(0),
            i4(10),
            i4(403),
            t("i"),
            i2(idx.columns.len() as i16),
            b(false),
            b(false),
            b(false),
            b(false),
            b(false),
            t("p"),
            b(false),
            SqlValue::Float4(0.0),
            i4(1),
            i2(0),
            i4(0),
        ]);
    }
    for v in catalog.views() {
        rows.push(vec![
            i4(v.oid as i32), t(&v.name), i4(schema_oid(catalog, &v.schema)), i4(0), i4(10), i4(0),
            t("v"), i2(v.columns.len() as i16), b(false), b(false), b(false), b(false), b(false),
            t("p"), b(false), SqlValue::Float4(0.0), i4(0), i2(0), i4(0),
        ]);
    }
    rs(cols, rows)
}

fn pg_attribute(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("attrelid", SqlType::Integer),
        ("attname", SqlType::Text),
        ("atttypid", SqlType::Integer),
        ("attstattarget", SqlType::Integer),
        ("attlen", SqlType::SmallInt),
        ("attnum", SqlType::SmallInt),
        ("attndims", SqlType::Integer),
        ("atttypmod", SqlType::Integer),
        ("attnotnull", SqlType::Boolean),
        ("atthasdef", SqlType::Boolean),
        ("attisdropped", SqlType::Boolean),
        ("attislocal", SqlType::Boolean),
        ("attidentity", SqlType::Char(Some(1))),
        ("attgenerated", SqlType::Char(Some(1))),
        ("attcollation", SqlType::Integer),
    ];
    let mut rows = Vec::new();
    for table in catalog.tables() {
        for col in &table.columns {
            rows.push(vec![
                i4(table.oid as i32),
                t(&col.name),
                i4(col.ty.oid() as i32),
                i4(-1),
                i2(col.ty.type_len()),
                i2(col.ordinal as i16 + 1),
                i4(if matches!(col.ty, SqlType::Array(_)) { 1 } else { 0 }),
                i4(-1),
                b(!col.nullable),
                b(col.default.is_some()),
                b(false),
                b(true),
                t(if col.identity_sequence.is_some() { "d" } else { "" }),
                t(""),
                i4(0),
            ]);
        }
    }
    rs(cols, rows)
}

fn pg_type(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("typname", SqlType::Text),
        ("typnamespace", SqlType::Integer),
        ("typlen", SqlType::SmallInt),
        ("typtype", SqlType::Char(Some(1))),
        ("typcategory", SqlType::Char(Some(1))),
        ("typrelid", SqlType::Integer),
        ("typelem", SqlType::Integer),
        ("typarray", SqlType::Integer),
        ("typbasetype", SqlType::Integer),
        ("typnotnull", SqlType::Boolean),
        ("typdelim", SqlType::Char(Some(1))),
    ];
    let pg = schema_oid(catalog, "pg_catalog");
    let base = [
        (16, "bool", 1, "b"),
        (17, "bytea", -1, "b"),
        (20, "int8", 8, "N"),
        (21, "int2", 2, "N"),
        (23, "int4", 4, "N"),
        (25, "text", -1, "S"),
        (114, "json", -1, "U"),
        (700, "float4", 4, "N"),
        (701, "float8", 8, "N"),
        (1042, "bpchar", -1, "S"),
        (1043, "varchar", -1, "S"),
        (1082, "date", 4, "D"),
        (1083, "time", 8, "D"),
        (1114, "timestamp", 8, "D"),
        (1184, "timestamptz", 8, "D"),
        (1700, "numeric", -1, "N"),
        (2950, "uuid", 16, "U"),
        (3802, "jsonb", -1, "U"),
    ];
    let rows = base
        .iter()
        .map(|(oid, name, len, cat)| {
            vec![
                i4(*oid),
                t(name),
                i4(pg),
                i2(*len as i16),
                t("b"),
                t(cat),
                i4(0),
                i4(0),
                i4(0),
                i4(0),
                b(false),
                t(","),
            ]
        })
        .collect();
    rs(cols, rows)
}

fn pg_index(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("indexrelid", SqlType::Integer),
        ("indrelid", SqlType::Integer),
        ("indnatts", SqlType::SmallInt),
        ("indnkeyatts", SqlType::SmallInt),
        ("indisunique", SqlType::Boolean),
        ("indisprimary", SqlType::Boolean),
        ("indisclustered", SqlType::Boolean),
        ("indisvalid", SqlType::Boolean),
        ("indkey", SqlType::Text),
    ];
    let mut rows = Vec::new();
    for idx in catalog.indexes() {
        let table_oid = catalog
            .resolve_table_name(Some(&idx.schema), &idx.table)
            .and_then(|q| catalog.get_table(&q).map(|t| t.oid))
            .unwrap_or(0);
        let table = catalog
            .resolve_table_name(Some(&idx.schema), &idx.table)
            .and_then(|q| catalog.get_table(&q).cloned());
        let indkey: Vec<String> = idx
            .columns
            .iter()
            .map(|c| {
                table
                    .as_ref()
                    .and_then(|t| t.column_index(c))
                    .map(|i| (i + 1).to_string())
                    .unwrap_or_else(|| "0".into())
            })
            .collect();
        rows.push(vec![
            i4(idx.oid as i32),
            i4(table_oid as i32),
            i2(idx.columns.len() as i16),
            i2(idx.columns.len() as i16),
            b(idx.unique),
            b(idx.primary),
            b(false),
            b(true),
            t(&indkey.join(" ")),
        ]);
    }
    rs(cols, rows)
}

fn pg_constraint(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("conname", SqlType::Text),
        ("connamespace", SqlType::Integer),
        ("contype", SqlType::Char(Some(1))),
        ("condeferrable", SqlType::Boolean),
        ("condeferred", SqlType::Boolean),
        ("convalidated", SqlType::Boolean),
        ("conrelid", SqlType::Integer),
        ("confrelid", SqlType::Integer),
        ("conkey", SqlType::Array(Box::new(SqlType::SmallInt))),
        ("confkey", SqlType::Array(Box::new(SqlType::SmallInt))),
        ("confupdtype", SqlType::Char(Some(1))),
        ("confdeltype", SqlType::Char(Some(1))),
    ];
    let mut rows = Vec::new();
    let mut oid = 30000;
    for table in catalog.tables() {
        let nsoid = schema_oid(catalog, &table.schema);
        let colnums = |cols: &[String]| -> SqlValue {
            SqlValue::Array(
                cols.iter()
                    .map(|c| SqlValue::Int2(table.column_index(c).map(|i| i as i16 + 1).unwrap_or(0)))
                    .collect(),
            )
        };
        if let Some(pk) = &table.primary_key {
            rows.push(vec![i4(oid), t(&pk.name), i4(nsoid), t("p"), b(false), b(false), b(true), i4(table.oid as i32), i4(0), colnums(&pk.columns), SqlValue::Array(vec![]), t(" "), t(" ")]);
            oid += 1;
        }
        for u in &table.uniques {
            let name = if u.name.is_empty() { format!("{}_{}_key", table.name, u.columns.join("_")) } else { u.name.clone() };
            rows.push(vec![i4(oid), t(&name), i4(nsoid), t("u"), b(false), b(false), b(true), i4(table.oid as i32), i4(0), colnums(&u.columns), SqlValue::Array(vec![]), t(" "), t(" ")]);
            oid += 1;
        }
        for fk in &table.foreign_keys {
            let frelid = catalog
                .resolve_table_name(Some(&fk.ref_schema), &fk.ref_table)
                .and_then(|q| catalog.get_table(&q).map(|t| t.oid))
                .unwrap_or(0);
            rows.push(vec![i4(oid), t(&fk.name), i4(nsoid), t("f"), b(false), b(false), b(true), i4(table.oid as i32), i4(frelid as i32), colnums(&fk.columns), SqlValue::Array(vec![]), t(action_char(fk.on_update)), t(action_char(fk.on_delete))]);
            oid += 1;
        }
        for c in &table.checks {
            rows.push(vec![i4(oid), t(&c.name), i4(nsoid), t("c"), b(false), b(false), b(true), i4(table.oid as i32), i4(0), SqlValue::Array(vec![]), SqlValue::Array(vec![]), t(" "), t(" ")]);
            oid += 1;
        }
    }
    rs(cols, rows)
}

fn action_char(a: guardian_relational::catalog::ReferentialAction) -> &'static str {
    use guardian_relational::catalog::ReferentialAction::*;
    match a {
        NoAction => "a",
        Restrict => "r",
        Cascade => "c",
        SetNull => "n",
        SetDefault => "d",
    }
}

fn pg_database(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("datname", SqlType::Text),
        ("datdba", SqlType::Integer),
        ("encoding", SqlType::Integer),
        ("datcollate", SqlType::Text),
        ("datctype", SqlType::Text),
        ("datistemplate", SqlType::Boolean),
        ("datallowconn", SqlType::Boolean),
    ];
    let rows = vec![vec![
        i4(DB_OID),
        t(&catalog.database),
        i4(10),
        i4(6),
        t("en_US.UTF-8"),
        t("en_US.UTF-8"),
        b(false),
        b(true),
    ]];
    rs(cols, rows)
}

fn pg_indexes(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("schemaname", SqlType::Text),
        ("tablename", SqlType::Text),
        ("indexname", SqlType::Text),
        ("tablespace", SqlType::Text),
        ("indexdef", SqlType::Text),
    ];
    let rows = catalog
        .indexes()
        .map(|idx| {
            let unique = if idx.unique { "UNIQUE " } else { "" };
            let def = format!(
                "CREATE {unique}INDEX {} ON {}.{} USING btree ({})",
                idx.name, idx.schema, idx.table, idx.columns.join(", ")
            );
            vec![t(&idx.schema), t(&idx.table), t(&idx.name), null(), t(&def)]
        })
        .collect();
    rs(cols, rows)
}

fn pg_attrdef(catalog: &Catalog) -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("adrelid", SqlType::Integer),
        ("adnum", SqlType::SmallInt),
        ("adbin", SqlType::Text),
        ("adsrc", SqlType::Text),
    ];
    let mut rows = Vec::new();
    let mut oid = 40000;
    for table in catalog.tables() {
        for col in &table.columns {
            if let Some(def) = &col.default {
                rows.push(vec![i4(oid), i4(table.oid as i32), i2(col.ordinal as i16 + 1), t(def), t(def)]);
                oid += 1;
            }
        }
    }
    rs(cols, rows)
}

fn pg_roles() -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("rolname", SqlType::Text),
        ("rolsuper", SqlType::Boolean),
        ("rolcanlogin", SqlType::Boolean),
    ];
    rs(cols, vec![vec![i4(10), t("guardian"), b(true), b(true)]])
}

fn pg_am() -> RowSet {
    let cols = &[
        ("oid", SqlType::Integer),
        ("amname", SqlType::Text),
        ("amhandler", SqlType::Integer),
        ("amtype", SqlType::Char(Some(1))),
    ];
    rs(cols, vec![vec![i4(403), t("btree"), i4(0), t("i")], vec![i4(405), t("hash"), i4(0), t("i")]])
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn rs(cols: &[(&str, SqlType)], rows: Vec<Tuple>) -> RowSet {
    let fields = cols
        .iter()
        .map(|(name, ty)| FieldRef { table: None, name: (*name).to_string(), ty: ty.clone() })
        .collect();
    RowSet { schema: RowSchema::new(fields), rows }
}

fn empty(cols: &[(&str, SqlType)]) -> RowSet {
    rs(cols, Vec::new())
}

fn relabel(mut rs: RowSet, alias: &str) -> RowSet {
    for f in &mut rs.schema.fields {
        f.table = Some(alias.to_string());
    }
    rs
}

fn type_metrics(ty: &SqlType) -> (SqlValue, SqlValue, SqlValue) {
    match ty {
        SqlType::Varchar(Some(n)) | SqlType::Char(Some(n)) => (i4(*n as i32), null(), null()),
        SqlType::SmallInt => (null(), i4(16), i4(0)),
        SqlType::Integer => (null(), i4(32), i4(0)),
        SqlType::BigInt => (null(), i4(64), i4(0)),
        SqlType::Real => (null(), i4(24), null()),
        SqlType::DoublePrecision => (null(), i4(53), null()),
        SqlType::Numeric { precision, scale } => (
            null(),
            precision.map(|p| i4(p as i32)).unwrap_or(null()),
            scale.map(|s| i4(s as i32)).unwrap_or(null()),
        ),
        _ => (null(), null(), null()),
    }
}

fn t(s: &str) -> SqlValue {
    SqlValue::Text(s.to_string())
}
fn null() -> SqlValue {
    SqlValue::Null
}
fn i4(n: i32) -> SqlValue {
    SqlValue::Int4(n)
}
fn i2(n: i16) -> SqlValue {
    SqlValue::Int2(n)
}
fn b(x: bool) -> SqlValue {
    SqlValue::Bool(x)
}
