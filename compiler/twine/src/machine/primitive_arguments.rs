//! Strict primitive operands stay in the session heap while later arguments are
//! forced. Only their IDs live in continuation frames; borrowing happens once
//! the primitive is ready to run, with no evaluation through this view.
use super::{Heap, Value, ValueId};
use std::ops::Index;

pub(crate) struct PrimitiveArguments<'a> {
    heap: &'a Heap<Value>,
    ids: &'a [ValueId],
}

impl<'a> PrimitiveArguments<'a> {
    pub(super) fn new(heap: &'a Heap<Value>, ids: &'a [ValueId]) -> Self {
        Self { heap, ids }
    }
}

impl Index<usize> for PrimitiveArguments<'_> {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        &self.heap[self.ids[index]]
    }
}
