//! The `vector` extension: pgvector-compatible vector similarity support.
//!
//! Implements the [pgvector](https://github.com/pgvector/pgvector) surface
//! GuardianDB supports natively. The `vector` type itself lives in the core
//! ([`crate::relational::SqlType::Vector`] / [`SqlValue::Vector`], including
//! the `'[1,2,3]'` text form); this module provides the distance functions
//! (`l2_distance`, `inner_product`, `cosine_distance`, `l1_distance`), the
//! utility functions (`vector_dims`, `vector_norm`, `l2_normalize`), and the
//! distance operators `<->`, `<#>`, `<=>`, `<+>` (routed here by
//! [`super::dispatch_operator`]).
//!
//! Semantics follow pgvector: elements are `f32` but every distance
//! accumulates in `f64`; `<#>` returns the *negative* inner product (so that
//! ascending index order ranks the most similar vectors first); cosine
//! distance involving a zero vector is `NaN`; `l2_normalize` returns a zero
//! vector unchanged; and operands of different dimensions raise a
//! datatype-mismatch error mirroring pgvector's "different vector dimensions"
//! error. All functions are strict (NULL in, NULL out).

use super::{ExtCtx, ExtensionDef, RuntimeStrategy, any_null, arg_vector, no_such};
use crate::relational::SqlValue;
use crate::sql::error::{Result, SqlError};

pub static DEF: ExtensionDef = ExtensionDef {
    name: "vector",
    default_version: "0.8.1",
    comment: "vector data type and vector similarity functions \
              (pgvector-compatible; index access methods are engine-native)",
    requires: &[],
    functions: &[
        "l2_distance",
        "inner_product",
        "cosine_distance",
        "l1_distance",
        "vector_dims",
        "vector_norm",
        "l2_normalize",
    ],
    types: &["vector"],
    // RFC 0005 phase 1 (feature `vector-index`): tuning knobs for the
    // engine-native hnsw access method. Registered unconditionally so `SHOW`
    // and `SET` behave consistently; without the feature no plan reads them.
    gucs: &[
        // Search-time candidate list size (pgvector's knob, same default).
        super::GucSpec {
            name: "hnsw.ef_search",
            default: "40",
        },
        // Adaptive-growth ceiling for filtered top-k scans, as a multiple of
        // `hnsw.ef_search` (RFC 0005 §6.2); past it the planner falls back
        // to the exact scan.
        super::GucSpec {
            name: "hnsw.ef_growth_cap",
            default: "10",
        },
        // Selective-filter cutover: when an indexed equality's measured
        // candidate count is within `threshold × LIMIT`, the exact
        // index-scan path is used instead of ANN (RFC 0005 §6.2).
        super::GucSpec {
            name: "hnsw.selectivity_threshold",
            default: "10",
        },
    ],
    trusted: true,
    call: Some(call),
    strategy: RuntimeStrategy::Native,
};

/// Scalar-function entry point. All functions are strict: any SQL NULL
/// argument yields NULL.
fn call(_ctx: &ExtCtx, name: &str, args: &[SqlValue]) -> Result<SqlValue> {
    if any_null(args) {
        return Ok(SqlValue::Null);
    }
    match name {
        "l2_distance" => binary(args, name, l2_distance),
        "inner_product" => binary(args, name, inner_product),
        "cosine_distance" => binary(args, name, cosine_distance),
        "l1_distance" => binary(args, name, l1_distance),
        "vector_dims" => {
            let v = arg_vector(args, 0, name)?;
            Ok(SqlValue::Int4(v.len() as i32))
        }
        "vector_norm" => {
            let v = arg_vector(args, 0, name)?;
            Ok(SqlValue::Float8(norm(&v)))
        }
        "l2_normalize" => {
            let v = arg_vector(args, 0, name)?;
            Ok(SqlValue::Vector(l2_normalize(v)))
        }
        _ => Err(no_such(name)),
    }
}

/// Distance-operator entry point, routed by [`super::dispatch_operator`].
/// SQL NULL semantics: any NULL operand yields NULL.
pub fn operator(op: &str, left: &SqlValue, right: &SqlValue) -> Result<SqlValue> {
    if left.is_null() || right.is_null() {
        return Ok(SqlValue::Null);
    }
    let args = [left.clone(), right.clone()];
    match op {
        "<->" => binary(&args, op, l2_distance),
        // pgvector returns the *negative* inner product so that ascending
        // index order ranks the most similar vectors first.
        "<#>" => binary(&args, op, |a, b| -inner_product(a, b)),
        "<=>" => binary(&args, op, cosine_distance),
        "<+>" => binary(&args, op, l1_distance),
        _ => Err(no_such(op)),
    }
}

/// Extract the two vector arguments of `func`, enforce equal dimensions
/// (pgvector: "different vector dimensions"), and apply `f`.
fn binary(args: &[SqlValue], func: &str, f: impl Fn(&[f32], &[f32]) -> f64) -> Result<SqlValue> {
    let a = arg_vector(args, 0, func)?;
    let b = arg_vector(args, 1, func)?;
    if a.len() != b.len() {
        return Err(SqlError::DatatypeMismatch {
            column: String::new(),
            expected: format!("vector({})", a.len()),
            actual: format!("vector({})", b.len()),
        });
    }
    Ok(SqlValue::Float8(f(&a, &b)))
}

/// `sqrt(sum((a_i - b_i)^2))` with `f64` accumulation.
fn l2_distance(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        .sqrt()
}

/// `sum(a_i * b_i)` with `f64` accumulation.
fn inner_product(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

/// `1 - (a . b) / (|a| |b|)`; `NaN` when either norm is zero (pgvector
/// leaves the similarity of a zero vector undefined).
fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    let (na, nb) = (norm(a), norm(b));
    if na == 0.0 || nb == 0.0 {
        return f64::NAN;
    }
    1.0 - inner_product(a, b) / (na * nb)
}

/// `sum(|a_i - b_i|)` with `f64` accumulation.
fn l1_distance(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
        .sum()
}

/// Euclidean norm with `f64` accumulation.
fn norm(v: &[f32]) -> f64 {
    v.iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt()
}

/// `v / |v|`; a zero vector is returned unchanged (pgvector returns the
/// input when the norm is zero).
fn l2_normalize(v: Vec<f32>) -> Vec<f32> {
    let n = norm(&v);
    if n == 0.0 {
        return v;
    }
    v.into_iter().map(|x| (f64::from(x) / n) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relational::SqlType;
    use chrono::Utc;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn invoke(name: &str, args: &[SqlValue]) -> Result<SqlValue> {
        let vars = RefCell::new(HashMap::new());
        let ctx = ExtCtx {
            now: Utc::now(),
            vars: &vars,
        };
        call(&ctx, name, args)
    }

    fn vector(items: &[f32]) -> SqlValue {
        SqlValue::Vector(items.to_vec())
    }

    fn f8(v: SqlValue) -> f64 {
        match v {
            SqlValue::Float8(f) => f,
            other => panic!("expected float8, got {other:?}"),
        }
    }

    #[test]
    fn l2_distance_exact() {
        let args = [vector(&[1.0, 2.0]), vector(&[4.0, 6.0])];
        assert_eq!(f8(invoke("l2_distance", &args).unwrap()), 5.0);
    }

    #[test]
    fn inner_product_exact() {
        let args = [vector(&[1.0, 2.0]), vector(&[3.0, 4.0])];
        assert_eq!(f8(invoke("inner_product", &args).unwrap()), 11.0);
    }

    #[test]
    fn inner_product_operator_is_negated() {
        let d = operator("<#>", &vector(&[1.0, 2.0]), &vector(&[3.0, 4.0]));
        assert_eq!(f8(d.unwrap()), -11.0);
    }

    #[test]
    fn cosine_distance_orthogonal_is_one() {
        let args = [vector(&[1.0, 0.0]), vector(&[0.0, 1.0])];
        assert_eq!(f8(invoke("cosine_distance", &args).unwrap()), 1.0);
    }

    #[test]
    fn cosine_distance_parallel_is_zero() {
        let args = [vector(&[1.0, 1.0]), vector(&[2.0, 2.0])];
        assert!(f8(invoke("cosine_distance", &args).unwrap()).abs() < 1e-9);
    }

    #[test]
    fn cosine_distance_zero_vector_is_nan() {
        let args = [vector(&[0.0, 0.0]), vector(&[1.0, 2.0])];
        assert!(f8(invoke("cosine_distance", &args).unwrap()).is_nan());
    }

    #[test]
    fn l1_distance_exact() {
        let args = [vector(&[1.0, 2.0]), vector(&[4.0, 6.0])];
        assert_eq!(f8(invoke("l1_distance", &args).unwrap()), 7.0);
    }

    #[test]
    fn vector_dims_reports_len() {
        match invoke("vector_dims", &[vector(&[1.0, 2.0, 3.0])]).unwrap() {
            SqlValue::Int4(n) => assert_eq!(n, 3),
            other => panic!("expected int4, got {other:?}"),
        }
    }

    #[test]
    fn vector_norm_exact() {
        assert_eq!(
            f8(invoke("vector_norm", &[vector(&[3.0, 4.0])]).unwrap()),
            5.0
        );
    }

    #[test]
    fn l2_normalize_exact() {
        match invoke("l2_normalize", &[vector(&[3.0, 4.0])]).unwrap() {
            SqlValue::Vector(v) => assert_eq!(v, [0.6, 0.8]),
            other => panic!("expected vector, got {other:?}"),
        }
    }

    #[test]
    fn l2_normalize_zero_vector_unchanged() {
        match invoke("l2_normalize", &[vector(&[0.0, 0.0])]).unwrap() {
            SqlValue::Vector(v) => assert_eq!(v, [0.0, 0.0]),
            other => panic!("expected vector, got {other:?}"),
        }
    }

    #[test]
    fn dimension_mismatch_errors() {
        let err = invoke("l2_distance", &[vector(&[1.0]), vector(&[1.0, 2.0])]).unwrap_err();
        match err {
            SqlError::DatatypeMismatch {
                column,
                expected,
                actual,
            } => {
                assert!(column.is_empty());
                assert_eq!(expected, "vector(1)");
                assert_eq!(actual, "vector(2)");
            }
            other => panic!("expected datatype mismatch, got {other:?}"),
        }
    }

    #[test]
    fn null_arguments_yield_null() {
        let v = vector(&[1.0]);
        assert!(
            invoke("l2_distance", &[SqlValue::Null, v.clone()])
                .unwrap()
                .is_null()
        );
        assert!(invoke("l2_normalize", &[SqlValue::Null]).unwrap().is_null());
        assert!(operator("<->", &SqlValue::Null, &v).unwrap().is_null());
        assert!(operator("<=>", &v, &SqlValue::Null).unwrap().is_null());
        assert!(
            operator("<#>", &SqlValue::Null, &SqlValue::Null)
                .unwrap()
                .is_null()
        );
    }

    #[test]
    fn operators_match_functions() {
        let a = vector(&[1.0, 2.0]);
        let b = vector(&[4.0, 6.0]);
        assert_eq!(f8(operator("<->", &a, &b).unwrap()), 5.0);
        assert_eq!(f8(operator("<+>", &a, &b).unwrap()), 7.0);
        let (x, y) = (vector(&[1.0, 0.0]), vector(&[0.0, 1.0]));
        assert_eq!(f8(operator("<=>", &x, &y).unwrap()), 1.0);
    }

    #[test]
    fn unknown_operator_errors() {
        assert!(operator("<?>", &vector(&[1.0]), &vector(&[1.0])).is_err());
    }

    #[test]
    fn text_arguments_coerce_to_vector() {
        let args = [
            SqlValue::Text("[1,2]".into()),
            SqlValue::Text("[4,6]".into()),
        ];
        assert_eq!(f8(invoke("l2_distance", &args).unwrap()), 5.0);
        match invoke("vector_dims", &[SqlValue::Text("[1,2,3]".into())]).unwrap() {
            SqlValue::Int4(n) => assert_eq!(n, 3),
            other => panic!("expected int4, got {other:?}"),
        }
    }

    #[test]
    fn every_registered_function_is_routed() {
        let args = [vector(&[1.0, 2.0]), vector(&[3.0, 4.0])];
        for name in DEF.functions {
            assert!(invoke(name, &args).is_ok(), "{name} not routed");
        }
    }

    #[test]
    fn vector_text_round_trip() {
        let parsed = SqlValue::from_text("[1, 2.5, 3]", &SqlType::Vector(None)).unwrap();
        match &parsed {
            SqlValue::Vector(v) => assert_eq!(v, &[1.0, 2.5, 3.0]),
            other => panic!("expected vector, got {other:?}"),
        }
        assert_eq!(parsed.to_text().unwrap(), "[1,2.5,3]");
    }

    #[test]
    fn vector_text_rejects_wrong_dimension() {
        assert!(matches!(
            SqlValue::from_text("[1,2,3]", &SqlType::Vector(Some(2))),
            Err(SqlError::DatatypeMismatch { .. })
        ));
    }
}
