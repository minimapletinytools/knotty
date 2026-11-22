//! Hashing for compiler/runtime-generated integer identifiers.
use std::hash::Hasher;

// Keys are internal, sequential arena indices, never user-controlled hash keys.
// A word mixer avoids SipHash on every edge of a transferred graph.
#[derive(Default)]
pub(super) struct IndexHasher(u64);
impl Hasher for IndexHasher {
    fn write(&mut self, _: &[u8]) {
        unreachable!("index maps only hash compiler/runtime IDs")
    }
    fn write_usize(&mut self, value: usize) {
        self.0 = (value as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
