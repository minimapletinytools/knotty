//! Stable expression IDs with cheap program forks. Appending or mutating a fork
//! copies only the affected chunk; serialization remains the original flat list.
use crate::Expr;
use serde::{ser::SerializeSeq, Deserialize, Deserializer, Serialize, Serializer};
use std::{
    ops::{Index, IndexMut},
    sync::{Arc, OnceLock},
};
const CHUNK: usize = 256;
#[derive(Debug, Clone, Default)]
pub struct Expressions {
    chunks: Vec<Arc<Vec<Expr>>>,
    len: usize,
    captures: OnceLock<Arc<crate::free_inputs::Captures>>,
    capture_prefix: Option<Arc<crate::free_inputs::Captures>>,
}
type Iter<'a> = std::iter::FlatMap<
    std::slice::Iter<'a, Arc<Vec<Expr>>>,
    std::slice::Iter<'a, Expr>,
    fn(&'a Arc<Vec<Expr>>) -> std::slice::Iter<'a, Expr>,
>;
type IterMut<'a> = std::iter::FlatMap<
    std::slice::IterMut<'a, Arc<Vec<Expr>>>,
    std::slice::IterMut<'a, Expr>,
    fn(&'a mut Arc<Vec<Expr>>) -> std::slice::IterMut<'a, Expr>,
>;
fn chunk_iter(chunk: &Arc<Vec<Expr>>) -> std::slice::Iter<'_, Expr> {
    chunk.iter()
}
fn chunk_iter_mut(chunk: &mut Arc<Vec<Expr>>) -> std::slice::IterMut<'_, Expr> {
    Arc::make_mut(chunk).iter_mut()
}
impl Expressions {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn push(&mut self, expr: Expr) {
        if let Some(captures) = self.captures.take() {
            self.capture_prefix = Some(captures);
        }
        if self.len.is_multiple_of(CHUNK) {
            self.chunks.push(Arc::new(Vec::with_capacity(CHUNK)));
        }
        Arc::make_mut(self.chunks.last_mut().unwrap()).push(expr);
        self.len += 1;
    }
    pub fn get(&self, index: usize) -> Option<&Expr> {
        self.chunks.get(index / CHUNK)?.get(index % CHUNK)
    }
    pub fn iter(&self) -> Iter<'_> {
        self.chunks.iter().flat_map(chunk_iter)
    }
    /// Prepared once per immutable expression arena, shared by subsequent forks.
    /// Appending preserves proved prefix layouts; other mutable access invalidates them.
    pub fn capture_layouts(&self) -> &crate::free_inputs::Captures {
        self.captures.get_or_init(|| {
            Arc::new(crate::free_inputs::Captures::prepare(
                self,
                self.capture_prefix.as_deref(),
            ))
        })
    }
    /// Old executable payload copied by the first append to a cloned arena.
    /// Directory entries are separate; complete chunks remain shared.
    pub fn append_copy_prefix(&self) -> impl Iterator<Item = &Expr> {
        self.iter_from(self.len - self.len % CHUNK)
    }
    /// Iterate an appended suffix without visiting chunks in the old prefix.
    pub fn iter_from(&self, start: usize) -> impl Iterator<Item = &Expr> {
        assert!(start <= self.len, "expression suffix outside program");
        self.chunks[start / CHUNK..]
            .iter()
            .enumerate()
            .flat_map(move |(index, chunk)| {
                chunk[if index == 0 { start % CHUNK } else { 0 }..].iter()
            })
    }
}
impl Index<usize> for Expressions {
    type Output = Expr;
    fn index(&self, index: usize) -> &Expr {
        self.get(index).expect("expression ID outside program")
    }
}
impl IndexMut<usize> for Expressions {
    fn index_mut(&mut self, index: usize) -> &mut Expr {
        assert!(index < self.len, "expression ID outside program");
        self.captures.take();
        if self
            .capture_prefix
            .as_ref()
            .is_some_and(|prefix| !prefix.precedes(index))
        {
            self.capture_prefix = None;
        }
        &mut Arc::make_mut(&mut self.chunks[index / CHUNK])[index % CHUNK]
    }
}
impl<'a> IntoIterator for &'a Expressions {
    type Item = &'a Expr;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut Expressions {
    type Item = &'a mut Expr;
    type IntoIter = IterMut<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.captures.take();
        self.capture_prefix = None;
        self.chunks.iter_mut().flat_map(chunk_iter_mut)
    }
}
impl Serialize for Expressions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for expr in self {
            sequence.serialize_element(expr)?;
        }
        sequence.end()
    }
}
impl<'de> Deserialize<'de> for Expressions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut arena = Self::default();
        for expr in Vec::<Expr>::deserialize(deserializer)? {
            arena.push(expr);
        }
        Ok(arena)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forks_share_prefixes_but_append_and_origin_rewriting_are_isolated() {
        let checked = knot_checker::check::check_library_program("Prelude").unwrap();
        let base = crate::lower(&[checked]);
        assert!(base.expressions.len() > CHUNK);
        let mut fork = base.clone();
        assert!(Arc::ptr_eq(
            &base.expressions.chunks[0],
            &fork.expressions.chunks[0]
        ));
        let first = base.expressions[0].clone();
        let id = fork.push(first.origin, crate::Kind::Unsupported("fork only".into()));
        assert_eq!(id.0, base.expressions.len());
        assert!(base.expressions.get(id.0).is_none());
        assert!(Arc::ptr_eq(
            &base.expressions.chunks[0],
            &fork.expressions.chunks[0]
        ));
        fork.expressions[0].origin.role = crate::Role::Synthetic;
        assert_eq!(base.expressions[0].origin.role, first.origin.role);
        assert!(Arc::ptr_eq(
            &base.expressions.chunks[1],
            &fork.expressions.chunks[1]
        ));
        for expr in &mut fork.expressions {
            expr.origin.role = crate::Role::Synthetic;
        }
        assert_eq!(base.expressions[0].origin.role, first.origin.role);
        assert!(!Arc::ptr_eq(
            &base.expressions.chunks[0],
            &fork.expressions.chunks[0]
        ));
        assert_eq!(fork.expressions.iter().count(), base.expressions.len() + 1);
        for start in [0, CHUNK - 1, CHUNK, CHUNK + 1, fork.expressions.len()] {
            assert_eq!(
                fork.expressions.iter_from(start).count(),
                fork.expressions.len() - start
            );
            assert!(fork
                .expressions
                .iter_from(start)
                .zip(fork.expressions.iter().skip(start))
                .all(|(actual, expected)| std::ptr::eq(actual, expected)));
        }
    }
}
