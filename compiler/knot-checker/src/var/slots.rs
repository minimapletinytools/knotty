//! Cloned substitutions share unchanged pages, including the prelude seed.
//! Writes detach only their page; indices and union-find semantics stay local
//! to the substitution. There is no global interner or additional cache.
use super::Slot;
use std::ops::{Index, IndexMut};
use std::sync::Arc;

// Limits the copied region per mutation and avoids a whole arena's geometric
// spare capacity. A partially filled page still reserves at most 256 slots.
const PAGE_SIZE: usize = 256;

#[derive(Debug)]
struct Page(Vec<Slot>);
impl Default for Page {
    fn default() -> Self {
        Self(Vec::with_capacity(PAGE_SIZE))
    }
}
impl Clone for Page {
    fn clone(&self) -> Self {
        let mut page = Self::default();
        page.0.extend(self.0.iter().cloned());
        page
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct Slots {
    pages: Vec<Arc<Page>>,
    len: usize,
}
impl Slots {
    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn push(&mut self, value: Slot) {
        if self.len.is_multiple_of(PAGE_SIZE) {
            self.pages.push(Arc::new(Page::default()));
        }
        Arc::make_mut(self.pages.last_mut().unwrap()).0.push(value);
        self.len += 1;
    }
}
impl Index<usize> for Slots {
    type Output = Slot;
    fn index(&self, index: usize) -> &Slot {
        &self.pages[index / PAGE_SIZE].0[index % PAGE_SIZE]
    }
}
impl IndexMut<usize> for Slots {
    fn index_mut(&mut self, index: usize) -> &mut Slot {
        &mut Arc::make_mut(&mut self.pages[index / PAGE_SIZE]).0[index % PAGE_SIZE]
    }
}
