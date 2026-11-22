//! Bounded ownership for immutable computation descriptions. Cache keys and
//! closures own support; this lookup holds only weak references and never results.
use super::{identity::Argument, CellId};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
};

#[derive(Debug)]
struct Budget {
    bytes: AtomicUsize,
    ceiling: usize,
}
#[derive(Debug)]
pub(super) struct Charge {
    budget: Arc<Budget>,
    bytes: usize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
pub(super) struct Store {
    budget: Arc<Budget>,
    entries: HashMap<CellId, Weak<Argument>>,
    order: VecDeque<CellId>,
    capacity: usize,
    local_ceiling: usize,
}
impl Store {
    pub fn new(bytes: usize, capacity: usize) -> Self {
        Self {
            budget: Arc::new(Budget {
                bytes: AtomicUsize::new(0),
                ceiling: bytes,
            }),
            entries: Default::default(),
            order: Default::default(),
            capacity,
            local_ceiling: usize::MAX,
        }
    }
    pub fn reserve(&self, bytes: usize) -> Option<Charge> {
        self.budget
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                let next = used.checked_add(bytes)?;
                (next <= self.budget.ceiling.min(self.local_ceiling)).then_some(next)
            })
            .ok()?;
        Some(Charge {
            budget: self.budget.clone(),
            bytes,
        })
    }
    pub fn set_local_ceiling(&mut self, ceiling: usize) -> usize {
        std::mem::replace(&mut self.local_ceiling, ceiling)
    }
    pub fn lookup(&self, cell: CellId) -> Option<Arc<Argument>> {
        self.entries.get(&cell)?.upgrade()
    }
    pub fn remember(&mut self, cell: CellId, recipe: &Arc<Argument>) {
        if self.capacity == 0 {
            return;
        }
        if let Some(entry) = self.entries.get_mut(&cell) {
            *entry = Arc::downgrade(recipe);
            return;
        }
        while self.entries.len() >= self.capacity {
            let oldest = self.order.pop_front().expect("bounded support lookup");
            self.entries.remove(&oldest);
        }
        self.order.push_back(cell);
        self.entries.insert(cell, Arc::downgrade(recipe));
    }
    pub fn bytes(&self) -> usize {
        self.budget.bytes.load(Ordering::Relaxed)
    }
    pub fn entries(&self) -> usize {
        self.entries.len()
    }
    pub fn ceiling(&self) -> usize {
        self.budget.ceiling
    }
    pub fn clone_work(&self) -> usize {
        self.entries.len().saturating_mul(2)
    }
    pub fn fork(&self, capacity: usize) -> Self {
        if capacity < self.entries.len() {
            return Self {
                budget: self.budget.clone(),
                entries: Default::default(),
                order: Default::default(),
                capacity,
                local_ceiling: usize::MAX,
            };
        }
        Self {
            budget: self.budget.clone(),
            entries: self.entries.clone(),
            order: self.order.clone(),
            capacity,
            local_ceiling: usize::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_charges_release_only_with_the_last_owner_and_never_exceed_the_ceiling() {
        let store = Store::new(100, 2);
        let sibling = store.fork(1);
        let charge = Arc::new(store.reserve(60).unwrap());
        let retained = charge.clone();
        assert!(sibling.reserve(41).is_none());
        drop(charge);
        assert_eq!(sibling.bytes(), 60);
        drop(retained);
        assert_eq!(store.bytes(), 0);
        assert!(sibling.reserve(100).is_some());
        assert_eq!(store.bytes(), 0);
    }
}
