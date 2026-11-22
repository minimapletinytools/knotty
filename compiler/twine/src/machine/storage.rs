//! Generation-checked, session-owned graph storage. Strong edges are integer
//! handles; cycles do not recurse during traversal or drop. Collection marks and
//! plans a sweep before deleting anything. Checkpoints journal allocations only.
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
pub(super) mod selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Id(NonZeroU64);
impl Id {
    fn slot(self) -> usize {
        (self.0.get() as u32 - 1) as usize
    }
    fn generation(self) -> u32 {
        (self.0.get() >> 32) as u32
    }
}
pub(super) trait Trace {
    fn edge_count(&self) -> usize;
    fn edge(&self, index: usize) -> Id;
    fn sweep_work(&self) -> u64 {
        1 + self.edge_count() as u64
    }
}
#[derive(Clone)]
struct Slot<T> {
    generation: u32,
    marked: u64,
    value: Option<T>,
}
#[derive(Clone)]
pub(super) struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
    generation: u32,
    epoch: u64,
    live: usize,
    journal: Option<Vec<Id>>,
}
impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            generation: 0,
            epoch: 0,
            live: 0,
            journal: None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionStatus {
    Collected,
    Truncated,
    Cancelled,
    CheckpointActive,
    InvalidEdge,
}
#[derive(Debug, Clone, Copy)]
pub struct Collection {
    pub status: CollectionStatus,
    pub work: u64,
    pub reclaimed: usize,
    pub remaining: usize,
}
impl<T> Arena<T> {
    pub fn clone_work(&self) -> usize {
        self.slots.len().saturating_add(self.free.len())
    }
    /// Mark/sweep scans slots; unlike snapshot copying it does not scan free IDs.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn get(&self, id: Id) -> Option<&T> {
        let slot = self.slots.get(id.slot())?;
        (slot.generation == id.generation())
            .then_some(slot.value.as_ref())
            .flatten()
    }
    pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
        let slot = self.slots.get_mut(id.slot())?;
        (slot.generation == id.generation())
            .then_some(slot.value.as_mut())
            .flatten()
    }
    pub fn entries(&self) -> impl Iterator<Item = (Id, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    Id(
                        NonZeroU64::new((u64::from(slot.generation) << 32) | (index as u64 + 1))
                            .unwrap(),
                    ),
                    value,
                )
            })
        })
    }
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|s| s.value.as_ref())
    }
    pub fn insert(&mut self, value: T, capacity: usize) -> Result<Id, T> {
        let Some(generation) = self.generation.checked_add(1) else {
            return Err(value);
        };
        if self.live >= capacity {
            return Err(value);
        }
        let index = self.free.last().copied().unwrap_or(self.slots.len());
        let Some(low) = u32::try_from(index).ok().and_then(|n| n.checked_add(1)) else {
            return Err(value);
        };
        let id = Id(NonZeroU64::new((u64::from(generation) << 32) | u64::from(low)).unwrap());
        self.generation = generation;
        let slot = Slot {
            generation,
            marked: 0,
            value: Some(value),
        };
        if self.free.pop().is_some() {
            self.slots[index] = slot;
        } else {
            self.slots.push(slot);
        }
        self.live += 1;
        if let Some(journal) = &mut self.journal {
            journal.push(id);
        }
        Ok(id)
    }
    pub fn checkpoint(&mut self) {
        assert!(self.journal.is_none(), "nested storage checkpoint");
        self.journal = Some(Vec::new());
    }
    pub fn commit(&mut self) {
        self.journal.take().expect("active storage checkpoint");
    }
    pub fn rollback(&mut self) {
        let journal = self.journal.take().expect("active storage checkpoint");
        for id in journal.into_iter().rev() {
            assert!(
                self.remove(id).is_some(),
                "checkpoint allocations cannot be collected"
            );
        }
        // The allocation generation never rolls back: discarded handles cannot
        // become valid when a slot is reused after Cancel.
    }
    fn remove(&mut self, id: Id) -> Option<T> {
        let slot = self.slots.get_mut(id.slot())?;
        if slot.generation != id.generation() {
            return None;
        }
        let value = slot.value.take()?;
        self.free.push(id.slot());
        self.live -= 1;
        Some(value)
    }
}
struct Budget<'a> {
    left: u64,
    cancellation: &'a AtomicBool,
}
impl Budget<'_> {
    fn tick(&mut self, work: u64) -> Result<(), CollectionStatus> {
        if self.cancellation.load(Ordering::Relaxed) {
            return Err(CollectionStatus::Cancelled);
        }
        self.left = self
            .left
            .checked_sub(work)
            .ok_or(CollectionStatus::Truncated)?;
        Ok(())
    }
}
impl<T: Trace> Trace for std::sync::Arc<T> {
    fn edge_count(&self) -> usize {
        (**self).edge_count()
    }
    fn edge(&self, index: usize) -> Id {
        (**self).edge(index)
    }
    fn sweep_work(&self) -> u64 {
        (**self).sweep_work()
    }
}
impl<T: Trace> Arena<T> {
    /// Roots may be weak/expired; strong edges from a live root may not be.
    /// No deletion occurs on cancellation/exhaustion before the sweep. Once its
    /// entire work allowance is reserved, the bounded sweep finishes atomically.
    pub fn collect(
        &mut self,
        roots: impl IntoIterator<Item = Id>,
        work: u64,
        cancellation: &AtomicBool,
        mut removed: impl FnMut(&T),
    ) -> Collection {
        let mut budget = Budget {
            left: work,
            cancellation,
        };
        let result = (|| {
            if self.journal.is_some() {
                return Err(CollectionStatus::CheckpointActive);
            }
            if self.epoch == u64::MAX {
                for slot in &mut self.slots {
                    budget.tick(1)?;
                    slot.marked = 0;
                }
                self.epoch = 0;
            }
            self.epoch += 1;
            let mut todo = Vec::new();
            for root in roots {
                budget.tick(1)?;
                if self.get(root).is_some() && self.slots[root.slot()].marked != self.epoch {
                    self.slots[root.slot()].marked = self.epoch;
                    todo.push(root.slot());
                }
            }
            while let Some(index) = todo.pop() {
                budget.tick(1)?;
                let count = self.slots[index].value.as_ref().unwrap().edge_count();
                for edge in 0..count {
                    budget.tick(1)?;
                    let child = self.slots[index].value.as_ref().unwrap().edge(edge);
                    if self.get(child).is_none() {
                        return Err(CollectionStatus::InvalidEdge);
                    }
                    if self.slots[child.slot()].marked != self.epoch {
                        self.slots[child.slot()].marked = self.epoch;
                        todo.push(child.slot());
                    }
                }
            }
            let mut garbage = Vec::new();
            let mut sweep_work = 0u64;
            for (index, slot) in self.slots.iter().enumerate() {
                budget.tick(1)?;
                if let Some(value) = &slot.value {
                    if slot.marked != self.epoch {
                        sweep_work = sweep_work
                            .checked_add(value.sweep_work())
                            .ok_or(CollectionStatus::Truncated)?;
                        garbage.push(Id(NonZeroU64::new(
                            (u64::from(slot.generation) << 32) | (index as u64 + 1),
                        )
                        .unwrap()));
                    }
                }
            }
            budget.tick(sweep_work)?;
            let count = garbage.len();
            for id in garbage {
                let value = self.remove(id).unwrap();
                removed(&value);
                drop(value);
            }
            Ok(count)
        })();
        let (status, reclaimed) = match result {
            Ok(n) => (CollectionStatus::Collected, n),
            Err(status) => (status, 0),
        };
        Collection {
            status,
            work: work - budget.left,
            reclaimed,
            remaining: self.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    struct Node(Vec<Id>);
    impl Trace for Node {
        fn edge_count(&self) -> usize {
            self.0.len()
        }
        fn edge(&self, index: usize) -> Id {
            self.0[index]
        }
    }
    #[test]
    fn cycles_are_traced_and_old_generations_never_alias_recycled_slots() {
        let mut arena = Arena::default();
        let a = arena.insert(Node(Vec::new()), 4).unwrap();
        let b = arena.insert(Node(vec![a]), 4).unwrap();
        arena.slots[a.slot()].value.as_mut().unwrap().0.push(b);
        let cancel = AtomicBool::new(false);
        assert_eq!(arena.collect([a], 100, &cancel, |_| {}).remaining, 2);
        assert_eq!(arena.collect([], 100, &cancel, |_| {}).reclaimed, 2);
        let c = arena.insert(Node(Vec::new()), 4).unwrap();
        assert!(arena.get(a).is_none() && arena.get(b).is_none());
        assert!(arena.get(c).is_some());
        arena.generation = u32::MAX;
        assert!(arena.insert(Node(Vec::new()), 4).is_err());
    }
    #[test]
    fn cancellation_budget_and_checkpoints_leave_the_graph_intact() {
        let mut arena = Arena::default();
        let a = arena.insert(Node(Vec::new()), 10).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            arena.collect([], 0, &cancel, |_| {}).status,
            CollectionStatus::Truncated
        );
        assert!(arena.get(a).is_some());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            arena.collect([], 100, &cancel, |_| {}).status,
            CollectionStatus::Cancelled
        );
        cancel.store(false, Ordering::Relaxed);
        arena.checkpoint();
        let provisional = arena.insert(Node(vec![a]), 10).unwrap();
        assert_eq!(
            arena.collect([], 100, &cancel, |_| {}).status,
            CollectionStatus::CheckpointActive
        );
        arena.rollback();
        let replacement = arena.insert(Node(vec![a]), 10).unwrap();
        assert_ne!(provisional, replacement);
        assert!(arena.get(provisional).is_none());
        assert!(arena.get(a).is_some());
    }
    #[test]
    fn thousands_of_collections_with_bounded_roots_reuse_storage() {
        let mut arena = Arena::default();
        let cancel = AtomicBool::new(false);
        let root = arena.insert(Node(Vec::new()), 4).unwrap();
        for _ in 0..10_000 {
            let old = arena.insert(Node(vec![root]), 4).unwrap();
            assert_eq!(arena.collect([root], 100, &cancel, |_| {}).reclaimed, 1);
            assert!(arena.get(old).is_none());
            assert!(arena.slots.len() <= 2);
        }
    }
    #[test]
    fn deep_graphs_use_a_worklist_and_marking_failure_never_sweeps() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let mut arena = Arena::default();
                let mut root = arena.insert(Node(Vec::new()), 30_000).unwrap();
                for _ in 0..20_000 {
                    root = arena.insert(Node(vec![root]), 30_000).unwrap();
                }
                let cancel = AtomicBool::new(false);
                assert_eq!(
                    arena.collect([root], 100, &cancel, |_| {}).status,
                    CollectionStatus::Truncated
                );
                assert_eq!(arena.len(), 20_001);
                assert_eq!(
                    arena.collect([root], 100_000, &cancel, |_| {}).remaining,
                    20_001
                );
                assert_eq!(arena.collect([], 100_000, &cancel, |_| {}).remaining, 0);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
