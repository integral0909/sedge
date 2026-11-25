//! An insertion-ordered hash map for Sedge `Map` values.
//!
//! Keys are Int, Bool or String. The map keeps the key's original value
//! next to a hashable copy, so iteration hands back the same objects that
//! were inserted. Removal leaves a tombstone, and the entries are compacted
//! once tombstones outnumber live entries.

use crate::value::Value;
use std::collections::HashMap;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Int(i64),
    Bool(bool),
    Str(String),
}

#[derive(Debug, Default)]
pub struct OrderedMap {
    entries: Vec<Option<(Value, Value)>>,
    index: HashMap<Key, usize>,
    len: usize,
}

impl OrderedMap {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, key: &Key) -> Option<Value> {
        let i = *self.index.get(key)?;
        self.entries[i].map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: Key, key_value: Value, value: Value) {
        if let Some(&i) = self.index.get(&key) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = value;
                return;
            }
        }
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((key_value, value)));
        self.len += 1;
    }

    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        let i = self.index.remove(key)?;
        let (_, v) = self.entries[i].take()?;
        self.len -= 1;
        if self.entries.len() > 16 && self.len * 2 < self.entries.len() {
            self.compact();
        }
        Some(v)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
        self.len = 0;
    }

    fn compact(&mut self) {
        let mut old_to_new = HashMap::new();
        let mut kept = Vec::with_capacity(self.len);
        for (i, e) in self.entries.drain(..).enumerate() {
            if let Some(e) = e {
                old_to_new.insert(i, kept.len());
                kept.push(Some(e));
            }
        }
        for v in self.index.values_mut() {
            *v = old_to_new[v];
        }
        self.entries = kept;
    }

    pub fn iter(&self) -> impl Iterator<Item = (Value, Value)> + '_ {
        self.entries.iter().filter_map(|e| *e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_insertion_order_through_removals() {
        let mut m = OrderedMap::default();
        for i in 0..40 {
            m.insert(Key::Int(i), Value::Int(i), Value::Int(i * 10));
        }
        for i in (0..40).filter(|i| i % 3 != 0) {
            assert!(m.remove(&Key::Int(i)).is_some());
        }
        m.insert(Key::Int(3), Value::Int(3), Value::Int(-1));
        m.insert(Key::Int(100), Value::Int(100), Value::Int(0));
        let keys: Vec<i64> = m.iter().map(|(k, _)| k.as_int()).collect();
        let mut expected: Vec<i64> = (0..40).filter(|i| i % 3 == 0).collect();
        expected.push(100);
        assert_eq!(keys, expected);
        assert_eq!(m.get(&Key::Int(3)).unwrap().as_int(), -1);
        assert_eq!(m.len(), expected.len());
        assert!(m.get(&Key::Int(4)).is_none());
    }
}
