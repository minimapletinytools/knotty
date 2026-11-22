//! Most call frames bind exactly one parameter. Keep that binding inline;
//! recursive let groups and larger patterns still use a map.
use super::{index_hash::IndexHasher, LocalId, ThunkId};
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

#[derive(Default, Clone)]
pub(super) enum Bindings {
    #[default]
    Empty,
    One(LocalId, ThunkId),
    Many(HashMap<LocalId, ThunkId, BuildHasherDefault<IndexHasher>>),
    // Only the parameter belongs to this lexical scope. Captures are an
    // execution shortcut, not extra inspector-visible bindings.
    #[cfg(feature = "compact-captures")]
    Captured(LocalId, ThunkId, std::sync::Arc<[(LocalId, ThunkId)]>),
}
impl Bindings {
    pub fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(..) => 1,
            Self::Many(map) => map.len(),
            #[cfg(feature = "compact-captures")]
            Self::Captured(..) => 1,
        }
    }
    pub fn get(&self, key: &LocalId) -> Option<&ThunkId> {
        match self {
            Self::Empty => None,
            Self::One(k, v) => (k == key).then_some(v),
            Self::Many(map) => map.get(key),
            #[cfg(feature = "compact-captures")]
            Self::Captured(k, v, inputs) => {
                if k == key {
                    Some(v)
                } else {
                    inputs
                        .binary_search_by_key(key, |(k, _)| *k)
                        .ok()
                        .map(|i| &inputs[i].1)
                }
            }
        }
    }
    #[cfg(feature = "incremental")]
    pub fn lexical_get(&self, key: &LocalId) -> Option<&ThunkId> {
        if let Self::Captured(local, value, _) = self {
            return (local == key).then_some(value);
        }
        self.get(key)
    }
    pub fn insert(&mut self, key: LocalId, value: ThunkId) {
        match self {
            Self::Empty => *self = Self::One(key, value),
            Self::One(k, v) if *k == key => *v = value,
            Self::One(k, v) => *self = Self::Many([(*k, *v), (key, value)].into_iter().collect()),
            Self::Many(map) => {
                map.insert(key, value);
            }
            #[cfg(feature = "compact-captures")]
            Self::Captured(..) => unreachable!("call scopes are immutable"),
        }
    }
    pub fn clear(&mut self) {
        *self = Self::Empty;
    }
    pub fn iter(&self) -> impl Iterator<Item = (&LocalId, &ThunkId)> {
        let (one, many) = match self {
            Self::Empty => (None, None),
            Self::One(k, v) => (Some((k, v)), None),
            Self::Many(map) => (None, Some(map)),
            #[cfg(feature = "compact-captures")]
            Self::Captured(k, v, _) => (Some((k, v)), None),
        };
        one.into_iter()
            .chain(many.into_iter().flat_map(|map| map.iter()))
    }
    #[cfg(feature = "compact-captures")]
    pub fn is_capture_boundary(&self) -> bool {
        matches!(self, Self::Captured(..))
    }
}
