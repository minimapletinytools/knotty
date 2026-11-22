//! Copy-on-write debug names, partitioned by stable local-ID ranges. Small
//! checked extensions copy their touched ranges rather than every old string.
//! The serialized representation remains the original ordered map.
use crate::LocalId;
use serde::{ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};
use std::{collections::BTreeMap, sync::Arc};

const CHUNK: usize = 256;

#[derive(Debug, Clone, Default)]
pub struct LocalNames {
    chunks: BTreeMap<usize, Arc<BTreeMap<LocalId, String>>>,
    len: usize,
}

impl LocalNames {
    pub fn get(&self, id: &LocalId) -> Option<&String> {
        self.chunks.get(&(id.0 / CHUNK))?.get(id)
    }
    pub fn insert(&mut self, id: LocalId, name: String) -> Option<String> {
        let chunk = self.chunks.entry(id.0 / CHUNK).or_default();
        let previous = Arc::make_mut(chunk).insert(id, name);
        if previous.is_none() {
            self.len += 1;
        }
        previous
    }
    pub fn remove(&mut self, id: &LocalId) -> Option<String> {
        let bucket = id.0 / CHUNK;
        let chunk = self.chunks.get_mut(&bucket)?;
        let previous = Arc::make_mut(chunk).remove(id);
        if previous.is_some() {
            self.len -= 1;
        }
        if chunk.is_empty() {
            self.chunks.remove(&bucket);
        }
        previous
    }
    pub fn iter(&self) -> impl Iterator<Item = (&LocalId, &String)> {
        self.chunks.values().flat_map(|chunk| chunk.iter())
    }
    pub fn keys(&self) -> impl Iterator<Item = &LocalId> {
        self.iter().map(|(id, _)| id)
    }
    /// Shared chunks are immutable; changed chunks must retain every old entry.
    /// This accepts new names anywhere in ID order, including gaps in old ranges.
    pub(crate) fn preserves(&self, before: &Self) -> bool {
        before.chunks.iter().all(|(bucket, old)| {
            self.chunks.get(bucket).is_some_and(|new| {
                Arc::ptr_eq(old, new) || old.iter().all(|(id, name)| new.get(id) == Some(name))
            })
        })
    }
}

impl Serialize for LocalNames {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (id, name) in self.iter() {
            map.serialize_entry(id, name)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for LocalNames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut names = Self::default();
        for (id, name) in BTreeMap::<LocalId, String>::deserialize(deserializer)? {
            names.insert(id, name);
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forks_share_untouched_ranges_and_check_every_changed_range() {
        let mut base = LocalNames::default();
        for id in [1, 3, CHUNK, CHUNK + 1, 4 * CHUNK, usize::MAX] {
            base.insert(LocalId(id), format!("name{id}"));
        }
        let mut next = base.clone();
        next.insert(LocalId(2), "gap".into());
        next.insert(LocalId(3 * CHUNK), "new range".into());
        assert!(next.preserves(&base));
        assert!(!base.preserves(&next));
        assert!(!Arc::ptr_eq(&base.chunks[&0], &next.chunks[&0]));
        assert!(Arc::ptr_eq(&base.chunks[&1], &next.chunks[&1]));
        assert_eq!(base.get(&LocalId(2)), None);
        assert_eq!(
            next.keys().map(|id| id.0).collect::<Vec<_>>(),
            [1, 2, 3, CHUNK, CHUNK + 1, 3 * CHUNK, 4 * CHUNK, usize::MAX]
        );
        for id in [1, CHUNK, 4 * CHUNK, usize::MAX] {
            let mut changed = next.clone();
            changed.insert(LocalId(id), "changed".into());
            assert!(!changed.preserves(&base));
            assert_eq!(base.get(&LocalId(id)), Some(&format!("name{id}")));
            let mut removed = next.clone();
            removed.remove(&LocalId(id));
            assert!(!removed.preserves(&base));
        }
        assert!(next.preserves(&LocalNames::default()));
    }
}
