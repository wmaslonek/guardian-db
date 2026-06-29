//! The intermediate tuple model used by the executor.
//!
//! A [`RowSchema`] describes the columns of an intermediate result (each with an
//! optional originating table/alias and a type). A `Tuple` is a positionally
//! aligned vector of [`SqlValue`]s.

use crate::error::{Result, SqlError};
use guardian_relational::{SqlType, SqlValue};

#[derive(Clone, Debug)]
pub struct FieldRef {
    /// The table or alias the column came from, if known.
    pub table: Option<String>,
    pub name: String,
    pub ty: SqlType,
}

#[derive(Clone, Debug, Default)]
pub struct RowSchema {
    pub fields: Vec<FieldRef>,
}

impl RowSchema {
    pub fn new(fields: Vec<FieldRef>) -> Self {
        Self { fields }
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Resolve a (possibly table-qualified) column reference to its index.
    pub fn resolve(&self, table: Option<&str>, column: &str) -> Result<usize> {
        let mut found: Option<usize> = None;
        for (i, f) in self.fields.iter().enumerate() {
            let name_matches = f.name == column;
            let table_matches = match table {
                None => true,
                Some(t) => f.table.as_deref() == Some(t),
            };
            if name_matches && table_matches {
                if found.is_some() {
                    return Err(SqlError::Syntax(format!(
                        "column reference \"{column}\" is ambiguous"
                    )));
                }
                found = Some(i);
            }
        }
        found.ok_or_else(|| {
            let q = match table {
                Some(t) => format!("{t}.{column}"),
                None => column.to_string(),
            };
            SqlError::UndefinedColumn(q)
        })
    }

    /// Concatenate two schemas (for joins).
    pub fn concat(&self, other: &RowSchema) -> RowSchema {
        let mut fields = self.fields.clone();
        fields.extend(other.fields.iter().cloned());
        RowSchema { fields }
    }
}

pub type Tuple = Vec<SqlValue>;

/// A materialized result set produced by the SELECT executor.
#[derive(Clone, Debug, Default)]
pub struct RowSet {
    pub schema: RowSchema,
    pub rows: Vec<Tuple>,
}
