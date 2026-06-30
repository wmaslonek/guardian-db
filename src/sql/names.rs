//! Identifier handling with PostgreSQL case-folding rules.
//!
//! Unquoted identifiers fold to lower case; quoted identifiers are preserved
//! verbatim. This matches PostgreSQL and is what TypeORM relies on.

use sqlparser::ast::{Ident, ObjectName};

/// Fold a single identifier per PostgreSQL rules.
pub fn ident_name(ident: &Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

/// Extract the dotted parts of an object name (already case-folded).
pub fn object_name_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .filter_map(|part| part.as_ident())
        .map(ident_name)
        .collect()
}

/// Split an object name into `(schema, name)`. A three-part name's leading
/// catalog/database component is ignored (PostgreSQL only allows the current db).
pub fn split_schema_table(name: &ObjectName) -> (Option<String>, String) {
    let parts = object_name_parts(name);
    match parts.len() {
        0 => (None, String::new()),
        1 => (None, parts[0].clone()),
        _ => {
            let n = parts[parts.len() - 1].clone();
            let s = parts[parts.len() - 2].clone();
            (Some(s), n)
        }
    }
}
