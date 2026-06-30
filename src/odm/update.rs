use crate::odm::error::{OdmError, Result};
use serde_json::{Map, Number, Value};
use std::collections::BTreeSet;

/// Applies MongoDB-style update operators (`$set`, `$unset`, `$inc`) to a JSON
/// document in place.
///
/// Returns `true` if the document was modified. Rejects replacement-style
/// updates (keys not starting with `$`), writes to `immutable_fields`, and
/// unsupported operators.
pub(crate) fn apply_update(
    document: &mut Value,
    operations: &Value,
    immutable_fields: &BTreeSet<String>,
) -> Result<bool> {
    let operations = operations.as_object().ok_or_else(|| {
        OdmError::InvalidUpdate("update operations must be a JSON object".to_string())
    })?;

    if operations.is_empty() {
        return Ok(false);
    }
    if operations.keys().any(|key| !key.starts_with('$')) {
        return Err(OdmError::InvalidUpdate(
            "replacement updates are not supported; use operators such as $set".to_string(),
        ));
    }

    let mut changed = false;
    for (operator, operand) in operations {
        match operator.as_str() {
            "$set" => {
                let fields = object_operand(operator, operand)?;
                for (path, value) in fields {
                    ensure_mutable(path, immutable_fields)?;
                    changed |= set_path(document, path, value.clone())?;
                }
            }
            "$unset" => {
                let fields = object_operand(operator, operand)?;
                for path in fields.keys() {
                    ensure_mutable(path, immutable_fields)?;
                    changed |= unset_path(document, path)?;
                }
            }
            "$inc" => {
                let fields = object_operand(operator, operand)?;
                for (path, increment) in fields {
                    ensure_mutable(path, immutable_fields)?;
                    changed |= increment_path(document, path, increment)?;
                }
            }
            other => {
                return Err(OdmError::InvalidUpdate(format!(
                    "unsupported update operator `{other}`"
                )));
            }
        }
    }
    Ok(changed)
}

fn object_operand<'a>(operator: &str, value: &'a Value) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| OdmError::InvalidUpdate(format!("{operator} expects an object")))
}

fn ensure_mutable(path: &str, immutable_fields: &BTreeSet<String>) -> Result<()> {
    if let Some(field) = immutable_fields
        .iter()
        .find(|field| path == field.as_str() || path.starts_with(&format!("{field}.")))
    {
        return Err(OdmError::ImmutableField(field.clone()));
    }
    Ok(())
}

fn set_path(document: &mut Value, path: &str, value: Value) -> Result<bool> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(OdmError::InvalidUpdate(format!(
            "invalid field path `{path}`"
        )));
    }

    let mut current = document;
    for segment in &segments[..segments.len() - 1] {
        let object = current.as_object_mut().ok_or_else(|| {
            OdmError::InvalidUpdate(format!("cannot traverse `{path}` through a non-object"))
        })?;
        current = object
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }

    let object = current
        .as_object_mut()
        .ok_or_else(|| OdmError::InvalidUpdate(format!("cannot set `{path}` on a non-object")))?;
    let key = segments
        .last()
        .expect("path has at least one segment")
        .to_string();
    let changed = object.get(&key) != Some(&value);
    object.insert(key, value);
    Ok(changed)
}

fn unset_path(document: &mut Value, path: &str) -> Result<bool> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(OdmError::InvalidUpdate(format!(
            "invalid field path `{path}`"
        )));
    }

    let mut current = document;
    for segment in &segments[..segments.len() - 1] {
        let Some(next) = current
            .as_object_mut()
            .and_then(|object| object.get_mut(*segment))
        else {
            return Ok(false);
        };
        current = next;
    }
    Ok(current.as_object_mut().is_some_and(|object| {
        object
            .remove(*segments.last().expect("path is non-empty"))
            .is_some()
    }))
}

fn increment_path(document: &mut Value, path: &str, increment: &Value) -> Result<bool> {
    if !increment.is_number() {
        return Err(OdmError::InvalidUpdate(format!(
            "$inc value for `{path}` must be numeric"
        )));
    }

    // Clone the current value to release the immutable borrow before `set_path` (mutable).
    let current = crate::odm::query::value_at_path(document, path).cloned();
    if let Some(ref value) = current
        && !value.is_number()
    {
        return Err(OdmError::InvalidUpdate(format!(
            "$inc target `{path}` must be numeric"
        )));
    }

    let result = add_json_numbers(current.as_ref(), increment, path)?;
    set_path(document, path, result)
}

/// Adds two JSON numbers, preserving the **integer** type when both operands are
/// integers (avoids, for example, `1024 + 1` becoming `1025.0`). Falls back to
/// `f64` only when an operand is fractional or when the integer sum would
/// overflow `i64`.
fn add_json_numbers(current: Option<&Value>, increment: &Value, path: &str) -> Result<Value> {
    let current_i = match current {
        None => Some(0i64), // A missing field is treated as the integer 0.
        Some(value) => value.as_i64(),
    };
    if let (Some(c), Some(i)) = (current_i, increment.as_i64())
        && let Some(sum) = c.checked_add(i)
    {
        return Ok(Value::Number(Number::from(sum)));
    }

    // Fractional path (or i64 overflow): use f64.
    let current_f = current.map_or(0.0, |value| value.as_f64().unwrap_or(0.0));
    let increment_f = increment.as_f64().ok_or_else(|| {
        OdmError::InvalidUpdate(format!("$inc value for `{path}` must be numeric"))
    })?;
    let number = Number::from_f64(current_f + increment_f).ok_or_else(|| {
        OdmError::InvalidUpdate(format!("$inc for `{path}` produced a non-finite number"))
    })?;
    Ok(Value::Number(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_immutable() -> BTreeSet<String> {
        BTreeSet::new()
    }

    // ─── $inc: type preservation (regression of the Number(1025.0) vs 1025 bug) ──

    #[test]
    fn inc_preserves_integer_type() {
        let mut doc = json!({ "counter": 1024 });
        let changed = apply_update(
            &mut doc,
            &json!({ "$inc": { "counter": 1 } }),
            &no_immutable(),
        )
        .unwrap();
        assert!(changed);
        // Must be the integer 1025, NOT the float 1025.0.
        assert_eq!(doc["counter"], json!(1025));
        assert!(
            doc["counter"].is_i64(),
            "expected integer, got {:?}",
            doc["counter"]
        );
    }

    #[test]
    fn inc_on_missing_field_starts_at_zero_integer() {
        let mut doc = json!({});
        apply_update(&mut doc, &json!({ "$inc": { "n": 5 } }), &no_immutable()).unwrap();
        assert_eq!(doc["n"], json!(5));
        assert!(doc["n"].is_i64());
    }

    #[test]
    fn inc_with_negative_integer() {
        let mut doc = json!({ "n": 10 });
        apply_update(&mut doc, &json!({ "$inc": { "n": -3 } }), &no_immutable()).unwrap();
        assert_eq!(doc["n"], json!(7));
        assert!(doc["n"].is_i64());
    }

    #[test]
    fn inc_with_float_operand_yields_float() {
        let mut doc = json!({ "n": 1 });
        apply_update(&mut doc, &json!({ "$inc": { "n": 0.5 } }), &no_immutable()).unwrap();
        assert_eq!(doc["n"], json!(1.5));
    }

    #[test]
    fn inc_on_float_target_yields_float() {
        let mut doc = json!({ "n": 2.5 });
        apply_update(&mut doc, &json!({ "$inc": { "n": 1 } }), &no_immutable()).unwrap();
        assert_eq!(doc["n"], json!(3.5));
    }

    #[test]
    fn inc_non_numeric_target_errors() {
        let mut doc = json!({ "n": "abc" });
        let err = apply_update(&mut doc, &json!({ "$inc": { "n": 1 } }), &no_immutable());
        assert!(matches!(err, Err(OdmError::InvalidUpdate(_))));
    }

    #[test]
    fn inc_non_numeric_operand_errors() {
        let mut doc = json!({ "n": 1 });
        let err = apply_update(&mut doc, &json!({ "$inc": { "n": "x" } }), &no_immutable());
        assert!(matches!(err, Err(OdmError::InvalidUpdate(_))));
    }

    // ─── $set / $unset ───────────────────────────────────────────────────────

    #[test]
    fn set_nested_path_creates_intermediate_objects() {
        let mut doc = json!({});
        apply_update(
            &mut doc,
            &json!({ "$set": { "a.b.c": 7 } }),
            &no_immutable(),
        )
        .unwrap();
        assert_eq!(doc["a"]["b"]["c"], json!(7));
    }

    #[test]
    fn set_same_value_reports_no_change() {
        let mut doc = json!({ "x": 1 });
        let changed =
            apply_update(&mut doc, &json!({ "$set": { "x": 1 } }), &no_immutable()).unwrap();
        assert!(!changed);
    }

    #[test]
    fn unset_removes_field() {
        let mut doc = json!({ "x": 1, "y": 2 });
        let changed =
            apply_update(&mut doc, &json!({ "$unset": { "x": "" } }), &no_immutable()).unwrap();
        assert!(changed);
        assert!(doc.get("x").is_none());
        assert_eq!(doc["y"], json!(2));
    }

    #[test]
    fn unset_missing_field_is_noop() {
        let mut doc = json!({ "y": 2 });
        let changed =
            apply_update(&mut doc, &json!({ "$unset": { "x": "" } }), &no_immutable()).unwrap();
        assert!(!changed);
    }

    // ─── Validations ─────────────────────────────────────────────────────────

    #[test]
    fn immutable_field_is_rejected() {
        let mut immutable = BTreeSet::new();
        immutable.insert("id".to_string());
        let mut doc = json!({ "id": "a" });
        let err = apply_update(&mut doc, &json!({ "$set": { "id": "b" } }), &immutable);
        assert!(matches!(err, Err(OdmError::ImmutableField(_))));
    }

    #[test]
    fn replacement_update_without_operator_is_rejected() {
        let mut doc = json!({ "x": 1 });
        let err = apply_update(&mut doc, &json!({ "x": 2 }), &no_immutable());
        assert!(matches!(err, Err(OdmError::InvalidUpdate(_))));
    }

    #[test]
    fn unsupported_operator_is_rejected() {
        let mut doc = json!({ "x": 1 });
        let err = apply_update(&mut doc, &json!({ "$push": { "x": 2 } }), &no_immutable());
        assert!(matches!(err, Err(OdmError::InvalidUpdate(_))));
    }

    #[test]
    fn empty_operations_report_no_change() {
        let mut doc = json!({ "x": 1 });
        let changed = apply_update(&mut doc, &json!({}), &no_immutable()).unwrap();
        assert!(!changed);
    }
}

fn add_numbers(left: &Number, right: &Number, path: &str) -> Result<Number> {
    // serde_json distinguishes integer and floating-point JSON numbers. Keep
    // integer arithmetic integer whenever both operands are integers instead
    // of routing every $inc through f64 (which turned 1024 + 1 into 1025.0).
    if let (Some(left), Some(right)) = (integer_value(left), integer_value(right)) {
        let next = left.checked_add(right).ok_or_else(|| {
            OdmError::InvalidUpdate(format!(
                "$inc for `{path}` overflowed the JSON integer range"
            ))
        })?;

        if let Ok(value) = i64::try_from(next) {
            return Ok(Number::from(value));
        }
        if let Ok(value) = u64::try_from(next) {
            return Ok(Number::from(value));
        }
        return Err(OdmError::InvalidUpdate(format!(
            "$inc for `{path}` overflowed the JSON integer range"
        )));
    }

    let next = left.as_f64().unwrap_or(f64::NAN) + right.as_f64().unwrap_or(f64::NAN);
    Number::from_f64(next).ok_or_else(|| {
        OdmError::InvalidUpdate(format!("$inc for `{path}` produced a non-finite number"))
    })
}

fn integer_value(number: &Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

#[cfg(test)]
mod tests {
    use super::apply_update;
    use crate::odm::error::OdmError;
    use serde_json::json;
    use std::collections::BTreeSet;

    #[test]
    fn inc_preserves_integer_representation() {
        let mut document = json!({ "counter": 1024 });
        let changed = apply_update(
            &mut document,
            &json!({ "$inc": { "counter": 1, "created": 2 } }),
            &BTreeSet::new(),
        )
        .unwrap();

        assert!(changed);
        assert_eq!(document["counter"], json!(1025));
        assert_eq!(document["created"], json!(2));
        assert!(document["counter"].as_i64().is_some());
    }

    #[test]
    fn inc_keeps_fractional_arithmetic_fractional() {
        let mut document = json!({ "counter": 1.25 });
        apply_update(
            &mut document,
            &json!({ "$inc": { "counter": 0.5 } }),
            &BTreeSet::new(),
        )
        .unwrap();

        assert_eq!(document["counter"], json!(1.75));
    }

    #[test]
    fn inc_reports_integer_overflow() {
        let mut document = json!({ "counter": u64::MAX });
        let error = apply_update(
            &mut document,
            &json!({ "$inc": { "counter": 1 } }),
            &BTreeSet::new(),
        )
        .unwrap_err();

        assert!(
            matches!(error, OdmError::InvalidUpdate(message) if message.contains("overflowed"))
        );
    }
}
