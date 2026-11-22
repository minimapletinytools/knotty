//! Copy-on-write pages for revision-local heap tables. Integer graph edges never
//! own pages, so cycles cannot recurse on drop or retain a parent revision.
use std::{
    ops::{Index, IndexMut},
    sync::Arc,
};
const PAGE: usize = 64;
pub(super) struct Heap<T> {
    pages: Vec<Arc<Vec<Arc<T>>>>,
    len: usize,
    copied: u64,
}
impl<T> Clone for Heap<T> {
    fn clone(&self) -> Self {
        Self {
            pages: self.pages.clone(),
            len: self.len,
            copied: 0,
        }
    }
}
impl<T> Default for Heap<T> {
    fn default() -> Self {
        Self {
            pages: Vec::new(),
            len: 0,
            copied: 0,
        }
    }
}
impl<T> Heap<T> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, index: usize) -> Option<&T> {
        self.pages
            .get(index / PAGE)?
            .get(index % PAGE)
            .map(AsRef::as_ref)
    }
    pub(super) fn weak(&self, index: usize) -> Option<std::sync::Weak<T>> {
        Some(Arc::downgrade(
            self.pages.get(index / PAGE)?.get(index % PAGE)?,
        ))
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.pages.iter().flat_map(|p| p.iter().map(AsRef::as_ref))
    }
    pub fn append_work(&self, count: usize) -> u64 {
        if count > 0
            && !self.len.is_multiple_of(PAGE)
            && Arc::strong_count(self.pages.last().unwrap()) > 1
        {
            (self.len % PAGE) as u64
        } else {
            0
        }
    }
    pub fn take_copy_work(&mut self) -> u64 {
        std::mem::take(&mut self.copied)
    }
    fn page_mut(&mut self, page: usize) -> &mut Vec<Arc<T>> {
        if Arc::strong_count(&self.pages[page]) > 1 {
            self.copied = self.copied.saturating_add(self.pages[page].len() as u64);
        }
        Arc::make_mut(&mut self.pages[page])
    }
    pub fn pages(&self) -> usize {
        self.pages.len()
    }
    pub fn shared_pages(&self) -> usize {
        self.pages
            .iter()
            .filter(|p| Arc::strong_count(p) > 1)
            .count()
    }
}
impl<T: Clone> Heap<T> {
    pub fn push(&mut self, value: T) {
        if self.len.is_multiple_of(PAGE) {
            self.pages.push(Arc::new(Vec::with_capacity(PAGE)));
        }
        self.page_mut(self.pages.len() - 1).push(Arc::new(value));
        self.len += 1;
    }
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        self.pages.truncate(len.div_ceil(PAGE));
        if !len.is_multiple_of(PAGE) {
            self.page_mut(self.pages.len() - 1).truncate(len % PAGE);
        }
        self.len = len;
    }
}
impl<T> Index<usize> for Heap<T> {
    type Output = T;
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("heap index")
    }
}
impl<T: Clone> IndexMut<usize> for Heap<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.len);
        let page = index / PAGE;
        let slot = index % PAGE;
        // A shared page implies its entries will become shared after the page
        // copy, even if the entry currently has only one Arc reference.
        if Arc::strong_count(&self.pages[page]) > 1
            || Arc::strong_count(&self.pages[page][slot]) > 1
        {
            self.copied = self.copied.saturating_add(1);
        }
        Arc::make_mut(&mut self.page_mut(page)[slot])
    }
}
impl<T: Clone> Extend<T> for Heap<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, values: I) {
        for value in values {
            self.push(value);
        }
    }
}
impl<T: Clone> FromIterator<T> for Heap<T> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        let mut heap = Self::default();
        heap.extend(values);
        heap
    }
}

impl super::Session {
    /// Exact-program/world fork only. No recipe is reinterpreted in a different
    /// executable revision. Completed and pending states share immutable pages;
    /// each mutation goes through make_mut and cannot alter another overlay.
    pub(super) fn snapshot(&self, limits: super::Limits, spent: &mut u64) -> Option<Self> {
        use super::*;
        *spent = 0;
        if limits.steps == 0 {
            return None;
        }
        *spent = 1;
        if self.abandoned
            || self.inspection.is_some()
            || self.tracking.is_some()
            || limits.cancellation.load(Ordering::Relaxed)
            || limits.primitive_bytes != self.limits.primitive_bytes
            || limits.decimal_power != self.limits.decimal_power
            || limits.dependency_bytes != self.identity_support.ceiling()
            || self.heap_entries() > limits.heap_entries
            || self.memo.len() > limits.application_cache_entries
            || self.stats.host_bytes > limits.host_bytes
            || self.evidence_stats().certificates > limits.dependency_records
            || self.evidence_stats().reads > limits.dependency_reads
            || self.evidence_stats().bytes > limits.dependency_bytes
        {
            return None;
        }
        let work = self
            .program
            .expressions
            .len()
            .saturating_add(self.values.len())
            .saturating_add(self.globals.len())
            .saturating_add(self.memo.len().saturating_mul(2))
            .saturating_add(self.evidence.clone_work())
            .saturating_add(self.validation.clone_work())
            .saturating_add(
                self.source_scopes
                    .as_ref()
                    .map_or(0, source_code::Scopes::clone_work),
            )
            .saturating_add(self.identity_support.clone_work())
            .saturating_add(self.heap_pages())
            .saturating_add(1);
        if work as u64 > limits.steps {
            return None;
        }
        // Same registry pointer is not an opt-in snapshot/version promise.
        // Reject unversioned host code even when an old demand never reached it.
        for expression in &self.program.expressions {
            *spent += 1;
            if (*spent & 255) == 0 && limits.cancellation.load(Ordering::Relaxed) {
                return None;
            }
            if let Kind::Host(name, _) = &expression.kind {
                self.hosts.function(name)?.version.as_ref()?;
            }
        }
        for value in self.values.iter() {
            *spent += 1;
            if (*spent & 255) == 0 && limits.cancellation.load(Ordering::Relaxed) {
                return None;
            }
            if match value {
                Value::Native(native) => native.snapshot_id().is_none(),
                Value::Io(..) => true,
                _ => false,
            } {
                return None;
            }
        }
        if limits.cancellation.load(Ordering::Relaxed) {
            return None;
        }
        *spent = work as u64;
        Some(Self {
            program: self.program.clone(),
            source_code: self.source_code.clone(),
            source_scopes: self.source_scopes.clone(),
            hosts: self.hosts.clone(),
            id: fresh_session_id(),
            thunks: self.thunks.clone(),
            values: self.values.clone(),
            envs: self.envs.clone(),
            globals: self.globals.clone(),
            memo: self.memo.clone(),
            evidence: self.evidence.clone(),
            evidence_pressure: Default::default(),
            validation: self.validation.fork(),
            identity_support: self.identity_support.fork(limits.application_cache_entries),
            global_refs: self.global_refs.clone(),
            limits,
            stats: Stats {
                steps: work as u64,
                maintenance_steps: work as u64,
                activations: self.envs.len() as u64,
                host_bytes: self.stats.host_bytes,
                ..Stats::default()
            },
            abandoned: false,
            inspection: None,
            tracking: None,
        })
    }
    pub(super) fn heap_pages(&self) -> usize {
        self.thunks.pages() + self.values.pages() + self.envs.pages() + self.global_refs.pages()
    }
    /// Diagnostics for the current shared-page implementation, not a retained
    /// byte count. Payload graphs and metadata may share allocations separately.
    pub fn shared_heap_pages(&self) -> usize {
        self.thunks.shared_pages()
            + self.values.shared_pages()
            + self.envs.shared_pages()
            + self.global_refs.shared_pages()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tightened_budget_does_not_reverse_the_work_clock() {
        use super::super::{Limits, Outcome, Session};
        let mut s = Session::new(Arc::new(knot_core::Program::default()), Limits::default());
        s.stats.steps = 100;
        s.restrict_execution(50, 1000);
        assert_eq!(s.tick(), Err(Outcome::BudgetExhausted));
        assert_eq!(s.stats.steps, 100);
    }
    #[test]
    fn mutation_append_and_truncate_leave_sibling_snapshots_unchanged() {
        let original: Heap<_> = (0..150).collect();
        let mut a = original.clone();
        let mut b = original.clone();
        assert_eq!(a.shared_pages(), 3);
        a[1] = 99;
        a.push(150);
        assert_eq!(a.shared_pages(), 1);
        b.truncate(65);
        b.push(1000);
        assert_eq!(
            original.iter().copied().collect::<Vec<_>>(),
            (0..150).collect::<Vec<_>>()
        );
        assert_eq!(a[1], 99);
        assert_eq!(a[150], 150);
        assert_eq!(b[1], 1);
        assert_eq!(b[65], 1000);
        b.truncate(64);
        b.push(2000);
        assert_eq!(b[64], 2000);
        assert_eq!(original[64], 64);
        b.truncate(0);
        b.push(42);
        assert_eq!(b[0], 42);
    }
    #[test]
    fn pages_release_without_retaining_a_chain_of_snapshots() {
        let mut heap: Heap<_> = (0..128).map(Arc::new).collect();
        let retired = Arc::downgrade(&heap[0]);
        for n in 0..1000 {
            let mut next = heap.clone();
            next[0] = Arc::new(n);
            heap = next;
            assert_eq!(heap.pages(), 2);
            assert_eq!(heap.shared_pages(), 0);
        }
        assert!(retired.upgrade().is_none());
    }
}
