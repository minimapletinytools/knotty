//! Ordered, copy-on-write executable bindings. Small linked extensions copy
//! touched key ranges, while unchanged chunks also prove prefix preservation.
//! Serialization retains the original flat ordered-map representation.
use crate::{ExprId, Global};
use serde::{ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, ops::Index, sync::Arc};

const CHUNK: usize = 64;
type Bindings = BTreeMap<Global, ExprId>;

#[derive(Debug, Clone, Default)]
pub struct Globals {
    // Keys are inclusive lower bounds. Removing a binding need not change its
    // chunk's bound; every stored name still lies below the next chunk's bound.
    chunks: BTreeMap<Global, Arc<Bindings>>,
    len: usize,
}

impl Globals {
    pub fn get(&self, name: &Global) -> Option<&ExprId> {
        self.chunks.range(..=name).next_back()?.1.get(name)
    }
    pub fn contains_key(&self, name: &Global) -> bool {
        self.get(name).is_some()
    }
    pub fn insert(&mut self, name: Global, id: ExprId) -> Option<ExprId> {
        let bound = match self.chunks.range(..=&name).next_back() {
            Some((bound, _)) => bound.clone(),
            None => {
                // Extend the first range downwards, including for descending
                // insertion order, instead of creating singleton chunks.
                let first = self
                    .chunks
                    .pop_first()
                    .map(|(_, chunk)| chunk)
                    .unwrap_or_default();
                self.chunks.insert(name.clone(), first);
                name.clone()
            }
        };
        let chunk = Arc::make_mut(self.chunks.get_mut(&bound).unwrap());
        let previous = chunk.insert(name, id);
        if previous.is_none() {
            self.len += 1;
        }
        if chunk.len() > 2 * CHUNK {
            let middle = chunk.keys().nth(CHUNK).unwrap().clone();
            let right = chunk.split_off(&middle);
            self.chunks.insert(middle, Arc::new(right));
        }
        previous
    }
    pub fn remove(&mut self, name: &Global) -> Option<ExprId> {
        let (bound, chunk) = self.chunks.range(..=name).next_back()?;
        if !chunk.contains_key(name) {
            return None;
        }
        let bound = bound.clone();
        let chunk = Arc::make_mut(self.chunks.get_mut(&bound).unwrap());
        let previous = chunk.remove(name);
        self.len -= 1;
        if chunk.is_empty() {
            self.chunks.remove(&bound);
        }
        previous
    }
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            chunks: self.chunks.values(),
            current: None,
        }
    }
    pub fn keys(&self) -> impl Iterator<Item = &Global> {
        self.iter().map(|(name, _)| name)
    }
    pub fn values(&self) -> impl Iterator<Item = &ExprId> {
        self.iter().map(|(_, id)| id)
    }
    /// New names in key order, only if every old binding remains unchanged.
    /// Shared chunks are immutable. Split/replaced ranges fall back to exact
    /// lookup, including new aliases referring to old expression IDs.
    pub(crate) fn additions_since(&self, before: &Self) -> Option<Vec<(Global, ExprId)>> {
        let shared = |a: &Self, bound: &Global, chunk: &Arc<Bindings>| {
            a.chunks
                .get(bound)
                .is_some_and(|other| Arc::ptr_eq(chunk, other))
        };
        for (bound, chunk) in &before.chunks {
            if !shared(self, bound, chunk)
                && chunk.iter().any(|(name, id)| self.get(name) != Some(id))
            {
                return None;
            }
        }
        let mut added = Vec::new();
        for (bound, chunk) in &self.chunks {
            if !shared(before, bound, chunk) {
                added.extend(
                    chunk
                        .iter()
                        .filter(|(name, _)| !before.contains_key(name))
                        .map(|(name, id)| (name.clone(), *id)),
                );
            }
        }
        Some(added)
    }
}

pub struct Iter<'a> {
    chunks: std::collections::btree_map::Values<'a, Global, Arc<Bindings>>,
    current: Option<std::collections::btree_map::Iter<'a, Global, ExprId>>,
}
impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Global, &'a ExprId);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(pair) = self.current.as_mut().and_then(Iterator::next) {
                return Some(pair);
            }
            self.current = Some(self.chunks.next()?.iter());
        }
    }
}
impl<'a> IntoIterator for &'a Globals {
    type Item = (&'a Global, &'a ExprId);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl Index<&Global> for Globals {
    type Output = ExprId;
    fn index(&self, name: &Global) -> &ExprId {
        self.get(name).expect("missing global")
    }
}
impl Serialize for Globals {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (name, id) in self {
            map.serialize_entry(name, id)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for Globals {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut globals = Self::default();
        for (name, id) in Bindings::deserialize(deserializer)? {
            globals.insert(name, id);
        }
        Ok(globals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(i: usize) -> Global {
        Global::Value(crate::prelude(&format!("value{i:05}")))
    }

    #[test]
    fn mutations_and_order_match_a_flat_map_across_splits_and_empty_ranges() {
        let mut map = Globals::default();
        let mut flat = Bindings::new();
        for i in (0..700).rev() {
            assert_eq!(
                map.insert(name(i), ExprId(i)),
                flat.insert(name(i), ExprId(i))
            );
        }
        let saved = map.clone();
        for i in (0..700).step_by(3) {
            assert_eq!(map.remove(&name(i)), flat.remove(&name(i)));
            assert_eq!(map.remove(&name(i)), None);
        }
        for i in 0..900 {
            assert_eq!(map.get(&name(i)), flat.get(&name(i)));
            if i % 5 == 0 {
                assert_eq!(
                    map.insert(name(i), ExprId(0)),
                    flat.insert(name(i), ExprId(0))
                );
            }
        }
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            flat.iter().collect::<Vec<_>>()
        );
        assert_eq!(map.len, flat.len());
        assert_eq!(saved.get(&name(0)), Some(&ExprId(0)));
        assert_eq!(saved.get(&name(5)), Some(&ExprId(5)));
        for key in flat.keys() {
            map.remove(key);
        }
        assert_eq!(map.len, 0);
        assert!(map.chunks.is_empty());
    }

    #[test]
    fn append_proof_accepts_aliases_and_splits_but_rejects_changed_or_missing_bindings() {
        let mut base = Globals::default();
        for i in (2..600).step_by(2) {
            base.insert(name(i), ExprId(i));
        }
        let mut next = base.clone();
        let additions = (1..700)
            .step_by(2)
            .map(|i| (name(i), ExprId(2)))
            .collect::<Vec<_>>();
        for (name, id) in &additions {
            next.insert(name.clone(), *id);
        }
        assert_eq!(next.additions_since(&base), Some(additions));
        assert_eq!(base.additions_since(&base), Some(vec![]));
        assert_eq!(base.additions_since(&next), None);
        for i in [2, 128, 598] {
            let mut changed = next.clone();
            changed.insert(name(i), ExprId(0));
            assert_eq!(changed.additions_since(&base), None);
            let mut removed = next.clone();
            removed.remove(&name(i));
            assert_eq!(removed.additions_since(&base), None);
        }
        let mut tail = base.clone();
        tail.insert(name(900), ExprId(2));
        let first = base.chunks.first_key_value().unwrap();
        assert!(Arc::ptr_eq(first.1, &tail.chunks[first.0]));
        assert_eq!(
            tail.additions_since(&base),
            Some(vec![(name(900), ExprId(2))])
        );
    }
}
