use crate::odm::error::{OdmError, Result};
use serde_json::{Map, Value};
use std::cmp::Ordering;

/// Resolves a dotted field path (e.g. `"a.b.c"`) within a JSON document,
/// returning a reference to the nested value if every segment exists.
pub(crate) fn value_at_path<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = document;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// Evaluates a MongoDB-style query object against a document, returning whether
/// the document matches. Supports logical operators (`$and`/`$or`/`$nor`) and
/// per-field operators (`$eq`, `$gt`, `$in`, `$exists`, ...).
pub(crate) fn matches_query(document: &Value, query: &Value) -> Result<bool> {
    let query = query
        .as_object()
        .ok_or_else(|| OdmError::InvalidQuery("query must be a JSON object".to_string()))?;
    matches_object(document, query)
}

fn matches_object(document: &Value, query: &Map<String, Value>) -> Result<bool> {
    for (field, condition) in query {
        match field.as_str() {
            "$and" => {
                let clauses = condition
                    .as_array()
                    .ok_or_else(|| OdmError::InvalidQuery("$and expects an array".to_string()))?;
                for clause in clauses {
                    if !matches_query(document, clause)? {
                        return Ok(false);
                    }
                }
            }
            "$or" => {
                let clauses = condition
                    .as_array()
                    .ok_or_else(|| OdmError::InvalidQuery("$or expects an array".to_string()))?;
                let mut matched = false;
                for clause in clauses {
                    if matches_query(document, clause)? {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return Ok(false);
                }
            }
            "$nor" => {
                let clauses = condition
                    .as_array()
                    .ok_or_else(|| OdmError::InvalidQuery("$nor expects an array".to_string()))?;
                for clause in clauses {
                    if matches_query(document, clause)? {
                        return Ok(false);
                    }
                }
            }
            key if key.starts_with('$') => {
                return Err(OdmError::InvalidQuery(format!(
                    "unsupported logical operator `{key}`"
                )));
            }
            _ => {
                let actual = value_at_path(document, field);
                if !matches_condition(actual, condition)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn matches_condition(actual: Option<&Value>, condition: &Value) -> Result<bool> {
    if let Some(operators) = condition.as_object()
        && operators.keys().any(|key| key.starts_with('$'))
    {
        for (operator, operand) in operators {
            let matched = match operator.as_str() {
                "$eq" => actual.is_some_and(|value| values_equal(value, operand)),
                "$ne" => actual.is_none_or(|value| !values_equal(value, operand)),
                "$gt" => compare(actual, operand, |ordering| ordering == Ordering::Greater),
                "$gte" => compare(actual, operand, |ordering| ordering != Ordering::Less),
                "$lt" => compare(actual, operand, |ordering| ordering == Ordering::Less),
                "$lte" => compare(actual, operand, |ordering| ordering != Ordering::Greater),
                "$in" => {
                    let values = operand.as_array().ok_or_else(|| {
                        OdmError::InvalidQuery("$in expects an array".to_string())
                    })?;
                    actual.is_some_and(|value| {
                        values
                            .iter()
                            .any(|candidate| values_equal(value, candidate))
                    })
                }
                "$nin" => {
                    let values = operand.as_array().ok_or_else(|| {
                        OdmError::InvalidQuery("$nin expects an array".to_string())
                    })?;
                    actual.is_none_or(|value| {
                        values
                            .iter()
                            .all(|candidate| !values_equal(value, candidate))
                    })
                }
                "$exists" => {
                    let expected = operand.as_bool().ok_or_else(|| {
                        OdmError::InvalidQuery("$exists expects a boolean".to_string())
                    })?;
                    actual.is_some() == expected
                }
                "$size" => {
                    let expected = operand.as_u64().ok_or_else(|| {
                        OdmError::InvalidQuery("$size expects a non-negative integer".to_string())
                    })? as usize;
                    actual
                        .and_then(Value::as_array)
                        .is_some_and(|items| items.len() == expected)
                }
                other => {
                    return Err(OdmError::InvalidQuery(format!(
                        "unsupported field operator `{other}`"
                    )));
                }
            };
            if !matched {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    Ok(actual.is_some_and(|value| values_equal(value, condition)))
}

/// Equality used by query operators: a direct match, or — when the actual value
/// is an array — a match against any of its elements (array containment).
fn values_equal(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    actual
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == expected))
}

/// Compares the actual and expected values (numbers or strings) and applies
/// `predicate` to the resulting ordering; returns `false` when uncomparable.
fn compare(
    actual: Option<&Value>,
    expected: &Value,
    predicate: impl FnOnce(Ordering) -> bool,
) -> bool {
    let Some(actual) = actual else {
        return false;
    };

    let ordering = match (actual, expected) {
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .zip(right.as_f64())
            .and_then(|(left, right)| left.partial_cmp(&right)),
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        _ => None,
    };
    ordering.is_some_and(predicate)
}
