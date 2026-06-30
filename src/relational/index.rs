//! In-memory secondary index structures and key derivation.
//!
//! Indexes are real, ordered structures (BTree-backed) that the executor consults
//! for point and range lookups and that enforce uniqueness. They are maintained by
//! the engine alongside each table's materialized view and rebuilt from storage on
//! refresh, mirroring GuardianDB's local-first "refresh then operate" model.

use crate::relational::value::SqlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;

/// Separator between composite key components. `0x1f` (unit separator) cannot
/// appear in the textual key forms produced by [`SqlValue::index_key`].
const SEP: char = '\u{1f}';

/// Build a composite index key from the ordered key column values.
///
/// Returns `None` when any component is SQL NULL, signalling that the row should
/// not participate in unique enforcement (PostgreSQL treats NULLs as distinct).
pub fn composite_key(values: &[SqlValue]) -> Option<String> {
    if values.iter().any(|v| v.is_null()) {
        return None;
    }
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.push_str(&v.index_key());
    }
    Some(out)
}

/// A composite key that *includes* NULLs (used for ordered range scans where NULL
/// ordering matters). NULLs sort last (PostgreSQL default for ASC).
pub fn ordered_key(values: &[SqlValue]) -> String {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        if v.is_null() {
            out.push('\u{10fffe}'); // sorts after any normal key component
        } else {
            out.push_str(&v.index_key());
        }
    }
    out
}

/// An ordered secondary index: composite key -> set of row ids.
#[derive(Debug, Default, Clone)]
pub struct SecondaryIndex {
    map: BTreeMap<String, BTreeSet<String>>,
    /// Whether the index enforces uniqueness.
    pub unique: bool,
}

impl SecondaryIndex {
    pub fn new(unique: bool) -> Self {
        Self { map: BTreeMap::new(), unique }
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn insert(&mut self, key: String, row_id: String) {
        self.map.entry(key).or_default().insert(row_id);
    }

    pub fn remove(&mut self, key: &str, row_id: &str) {
        if let Some(set) = self.map.get_mut(key) {
            set.remove(row_id);
            if set.is_empty() {
                self.map.remove(key);
            }
        }
    }

    /// Row ids for an exact key match.
    pub fn get(&self, key: &str) -> BTreeSet<String> {
        self.map.get(key).cloned().unwrap_or_default()
    }

    /// Returns the row id currently occupying `key`, if any (for unique checks).
    pub fn unique_occupant(&self, key: &str) -> Option<String> {
        self.map.get(key).and_then(|s| s.iter().next().cloned())
    }

    /// Row ids whose key falls within `[lo, hi]`.
    pub fn range(&self, lo: Bound<String>, hi: Bound<String>) -> BTreeSet<String> {
        self.map
            .range((lo, hi))
            .flat_map(|(_, set)| set.iter().cloned())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_key_skips_nulls() {
        assert!(composite_key(&[SqlValue::Int4(1), SqlValue::Null]).is_none());
        assert!(composite_key(&[SqlValue::Int4(1), SqlValue::Int4(2)]).is_some());
    }

    #[test]
    fn secondary_index_point_lookup() {
        let mut idx = SecondaryIndex::new(false);
        let k = composite_key(&[SqlValue::Text("a".into())]).unwrap();
        idx.insert(k.clone(), "row1".into());
        idx.insert(k.clone(), "row2".into());
        assert_eq!(idx.get(&k).len(), 2);
        idx.remove(&k, "row1");
        assert_eq!(idx.get(&k).len(), 1);
    }

    #[test]
    fn secondary_index_range() {
        let mut idx = SecondaryIndex::new(false);
        for n in 1..=5 {
            let k = ordered_key(&[SqlValue::Int4(n)]);
            idx.insert(k, format!("row{n}"));
        }
        let lo = ordered_key(&[SqlValue::Int4(2)]);
        let hi = ordered_key(&[SqlValue::Int4(4)]);
        let got = idx.range(Bound::Included(lo), Bound::Included(hi));
        assert_eq!(got.len(), 3);
    }
}
