//! PostgreSQL-compatible type system.
//!
//! [`SqlType`] enumerates the SQL types GuardianDB's relational layer understands.
//! Each type maps to a PostgreSQL type OID (used in the wire-protocol RowDescription
//! and ParameterDescription messages) and a canonical name.

use crate::error::{RelError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A SQL column/expression type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlType {
    Boolean,
    SmallInt,
    Integer,
    BigInt,
    Real,
    DoublePrecision,
    /// Arbitrary precision numeric. `precision`/`scale` are advisory.
    Numeric {
        precision: Option<u32>,
        scale: Option<u32>,
    },
    Text,
    Varchar(Option<u32>),
    Char(Option<u32>),
    Bytea,
    Uuid,
    Date,
    Time,
    Timestamp,
    Timestamptz,
    Json,
    Jsonb,
    Array(Box<SqlType>),
    /// `void`/unknown placeholder used for untyped NULLs and some expressions.
    Unknown,
}

impl SqlType {
    /// PostgreSQL type OID. These are the stable OIDs hard-coded in `pg_type`.
    pub fn oid(&self) -> u32 {
        match self {
            SqlType::Boolean => 16,
            SqlType::Bytea => 17,
            SqlType::Char(_) => 1042, // bpchar
            SqlType::BigInt => 20,
            SqlType::SmallInt => 21,
            SqlType::Integer => 23,
            SqlType::Text => 25,
            SqlType::Json => 114,
            SqlType::Real => 700,
            SqlType::DoublePrecision => 701,
            SqlType::Varchar(_) => 1043,
            SqlType::Date => 1082,
            SqlType::Time => 1083,
            SqlType::Timestamp => 1114,
            SqlType::Timestamptz => 1184,
            SqlType::Numeric { .. } => 1700,
            SqlType::Uuid => 2950,
            SqlType::Jsonb => 3802,
            SqlType::Array(inner) => inner.array_oid(),
            SqlType::Unknown => 705, // unknown
        }
    }

    /// The element OID when this type is used as an array element.
    fn array_oid(&self) -> u32 {
        match self {
            SqlType::Boolean => 1000,
            SqlType::Bytea => 1001,
            SqlType::Char(_) => 1014,
            SqlType::BigInt => 1016,
            SqlType::SmallInt => 1005,
            SqlType::Integer => 1007,
            SqlType::Text => 1009,
            SqlType::Json => 199,
            SqlType::Real => 1021,
            SqlType::DoublePrecision => 1022,
            SqlType::Varchar(_) => 1015,
            SqlType::Date => 1182,
            SqlType::Time => 1183,
            SqlType::Timestamp => 1115,
            SqlType::Timestamptz => 1185,
            SqlType::Numeric { .. } => 1231,
            SqlType::Uuid => 2951,
            SqlType::Jsonb => 3807,
            // nested arrays / unknown collapse to text[]
            _ => 1009,
        }
    }

    /// `typlen` used in `pg_type` / RowDescription (negative for variable length).
    pub fn type_len(&self) -> i16 {
        match self {
            SqlType::Boolean => 1,
            SqlType::SmallInt => 2,
            SqlType::Integer | SqlType::Real | SqlType::Date => 4,
            SqlType::BigInt | SqlType::DoublePrecision | SqlType::Time | SqlType::Timestamp
            | SqlType::Timestamptz => 8,
            SqlType::Uuid => 16,
            _ => -1,
        }
    }

    /// Canonical, lower-case PostgreSQL type name (as reported by `format_type`).
    pub fn name(&self) -> String {
        match self {
            SqlType::Boolean => "boolean".into(),
            SqlType::SmallInt => "smallint".into(),
            SqlType::Integer => "integer".into(),
            SqlType::BigInt => "bigint".into(),
            SqlType::Real => "real".into(),
            SqlType::DoublePrecision => "double precision".into(),
            SqlType::Numeric { precision, scale } => match (precision, scale) {
                (Some(p), Some(s)) => format!("numeric({p},{s})"),
                (Some(p), None) => format!("numeric({p})"),
                _ => "numeric".into(),
            },
            SqlType::Text => "text".into(),
            SqlType::Varchar(Some(n)) => format!("character varying({n})"),
            SqlType::Varchar(None) => "character varying".into(),
            SqlType::Char(Some(n)) => format!("character({n})"),
            SqlType::Char(None) => "character".into(),
            SqlType::Bytea => "bytea".into(),
            SqlType::Uuid => "uuid".into(),
            SqlType::Date => "date".into(),
            SqlType::Time => "time without time zone".into(),
            SqlType::Timestamp => "timestamp without time zone".into(),
            SqlType::Timestamptz => "timestamp with time zone".into(),
            SqlType::Json => "json".into(),
            SqlType::Jsonb => "jsonb".into(),
            SqlType::Array(inner) => format!("{}[]", inner.name()),
            SqlType::Unknown => "unknown".into(),
        }
    }

    /// The short name used by `information_schema.columns.data_type`.
    pub fn information_schema_name(&self) -> String {
        match self {
            SqlType::Varchar(_) => "character varying".into(),
            SqlType::Char(_) => "character".into(),
            SqlType::Array(_) => "ARRAY".into(),
            other => other.name(),
        }
    }

    /// `udt_name` used by `information_schema.columns` (matches pg internal names).
    pub fn udt_name(&self) -> String {
        match self {
            SqlType::Boolean => "bool".into(),
            SqlType::SmallInt => "int2".into(),
            SqlType::Integer => "int4".into(),
            SqlType::BigInt => "int8".into(),
            SqlType::Real => "float4".into(),
            SqlType::DoublePrecision => "float8".into(),
            SqlType::Numeric { .. } => "numeric".into(),
            SqlType::Text => "text".into(),
            SqlType::Varchar(_) => "varchar".into(),
            SqlType::Char(_) => "bpchar".into(),
            SqlType::Bytea => "bytea".into(),
            SqlType::Uuid => "uuid".into(),
            SqlType::Date => "date".into(),
            SqlType::Time => "time".into(),
            SqlType::Timestamp => "timestamp".into(),
            SqlType::Timestamptz => "timestamptz".into(),
            SqlType::Json => "json".into(),
            SqlType::Jsonb => "jsonb".into(),
            SqlType::Array(inner) => format!("_{}", inner.udt_name()),
            SqlType::Unknown => "unknown".into(),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            SqlType::SmallInt
                | SqlType::Integer
                | SqlType::BigInt
                | SqlType::Real
                | SqlType::DoublePrecision
                | SqlType::Numeric { .. }
        )
    }

    pub fn is_integer(&self) -> bool {
        matches!(self, SqlType::SmallInt | SqlType::Integer | SqlType::BigInt)
    }

    pub fn is_string(&self) -> bool {
        matches!(self, SqlType::Text | SqlType::Varchar(_) | SqlType::Char(_))
    }

    pub fn is_temporal(&self) -> bool {
        matches!(
            self,
            SqlType::Date | SqlType::Time | SqlType::Timestamp | SqlType::Timestamptz
        )
    }

    /// Parse a PostgreSQL type name (case-insensitive) into a [`SqlType`].
    pub fn parse(name: &str) -> Result<SqlType> {
        let lower = name.trim().replace('"', "").to_ascii_lowercase();
        // Strip an array suffix `[]` (one level supported explicitly, more collapse).
        if let Some(base) = lower.strip_suffix("[]") {
            return Ok(SqlType::Array(Box::new(SqlType::parse(base)?)));
        }
        // Separate the base name from a parenthesised modifier list.
        let (base, modifiers) = match lower.split_once('(') {
            Some((b, rest)) => {
                let inner = rest.trim_end_matches(')');
                (b.trim(), Some(inner))
            }
            None => (lower.as_str(), None),
        };
        let parse_mods = || -> Vec<u32> {
            modifiers
                .map(|m| {
                    m.split(',')
                        .filter_map(|x| x.trim().parse::<u32>().ok())
                        .collect()
                })
                .unwrap_or_default()
        };
        let ty = match base {
            "bool" | "boolean" => SqlType::Boolean,
            "int2" | "smallint" | "smallserial" | "serial2" => SqlType::SmallInt,
            "int" | "int4" | "integer" | "serial" | "serial4" => SqlType::Integer,
            "int8" | "bigint" | "bigserial" | "serial8" => SqlType::BigInt,
            "real" | "float4" => SqlType::Real,
            "double precision" | "float8" | "double" => SqlType::DoublePrecision,
            "float" => SqlType::DoublePrecision,
            "numeric" | "decimal" => {
                let m = parse_mods();
                SqlType::Numeric {
                    precision: m.first().copied(),
                    scale: m.get(1).copied(),
                }
            }
            "text" => SqlType::Text,
            "varchar" | "character varying" => SqlType::Varchar(parse_mods().first().copied()),
            "char" | "character" | "bpchar" => SqlType::Char(parse_mods().first().copied()),
            "bytea" => SqlType::Bytea,
            "uuid" => SqlType::Uuid,
            "date" => SqlType::Date,
            "time" | "time without time zone" => SqlType::Time,
            "timestamp" | "timestamp without time zone" => SqlType::Timestamp,
            "timestamptz" | "timestamp with time zone" => SqlType::Timestamptz,
            "json" => SqlType::Json,
            "jsonb" => SqlType::Jsonb,
            // PostgreSQL OID-alias and internal types: surfaced so casts/columns
            // referencing them resolve. `regclass` is handled specially by the
            // engine (name -> OID); the rest map to their natural storage type.
            "oid" | "regclass" | "regtype" | "regproc" | "regrole" | "regnamespace" | "xid"
            | "cid" => SqlType::Integer,
            "name" => SqlType::Text,
            other => return Err(RelError::UndefinedType(other.to_string())),
        };
        Ok(ty)
    }

    /// Does a column declared as `serial`/`bigserial`/`smallserial` map here?
    pub fn is_serial_name(name: &str) -> Option<SqlType> {
        match name.trim().to_ascii_lowercase().as_str() {
            "smallserial" | "serial2" => Some(SqlType::SmallInt),
            "serial" | "serial4" => Some(SqlType::Integer),
            "bigserial" | "serial8" => Some(SqlType::BigInt),
            _ => None,
        }
    }
}

impl fmt::Display for SqlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_types() {
        assert_eq!(SqlType::parse("INTEGER").unwrap(), SqlType::Integer);
        assert_eq!(SqlType::parse("int4").unwrap(), SqlType::Integer);
        assert_eq!(SqlType::parse("bigint").unwrap(), SqlType::BigInt);
        assert_eq!(
            SqlType::parse("varchar(255)").unwrap(),
            SqlType::Varchar(Some(255))
        );
        assert_eq!(
            SqlType::parse("numeric(10,2)").unwrap(),
            SqlType::Numeric { precision: Some(10), scale: Some(2) }
        );
        assert_eq!(SqlType::parse("timestamptz").unwrap(), SqlType::Timestamptz);
        assert_eq!(
            SqlType::parse("text[]").unwrap(),
            SqlType::Array(Box::new(SqlType::Text))
        );
        assert_eq!(SqlType::parse("jsonb").unwrap(), SqlType::Jsonb);
    }

    #[test]
    fn serial_maps_to_integer() {
        assert_eq!(SqlType::is_serial_name("serial"), Some(SqlType::Integer));
        assert_eq!(SqlType::is_serial_name("bigserial"), Some(SqlType::BigInt));
        assert_eq!(SqlType::parse("serial").unwrap(), SqlType::Integer);
    }

    #[test]
    fn oids_match_postgres() {
        assert_eq!(SqlType::Integer.oid(), 23);
        assert_eq!(SqlType::Text.oid(), 25);
        assert_eq!(SqlType::Boolean.oid(), 16);
        assert_eq!(SqlType::Uuid.oid(), 2950);
        assert_eq!(SqlType::Jsonb.oid(), 3802);
        assert_eq!(SqlType::Timestamptz.oid(), 1184);
        assert_eq!(SqlType::Array(Box::new(SqlType::Integer)).oid(), 1007);
    }

    #[test]
    fn unknown_type_errors() {
        assert!(matches!(
            SqlType::parse("frobnicate"),
            Err(RelError::UndefinedType(_))
        ));
    }
}
