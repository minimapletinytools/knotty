//! Session-local memoization identities under one combined entry bound.
//! Closed expressions point to ordinary thunks, which own pending/done state.
//! The control preserves finite expression identities during call churn. The
//! development engine makes all optional index kinds evictable under one bound.
//! Captured keys also point to ordinary lazy cells.
//! No index forces inputs or compares their values.
use super::{cost, CellId, ExprId, LocalId, ValueId};
use std::collections::{BTreeMap, HashMap, VecDeque};
#[derive(Clone, Debug)]
pub(super) struct Call {
    pub function: ValueId,
    pub argument: CellId,
    #[cfg(feature = "incremental")]
    pub identity: Option<std::sync::Arc<super::identity::Signature>>,
    #[cfg(feature = "incremental")]
    pub argument_identity: Option<std::sync::Arc<super::identity::Argument>>,
}
impl Call {
    pub fn allocated(function: ValueId, argument: CellId) -> Self {
        Self {
            function,
            argument,
            #[cfg(feature = "incremental")]
            identity: None,
            #[cfg(feature = "incremental")]
            argument_identity: None,
        }
    }
    pub fn slots(&self) -> usize {
        #[cfg(feature = "incremental")]
        {
            // Recursive support is charged once by its shared allocation owner.
            // This index pays for each immediate key, not every unfolded DAG path.
            let function = self.identity.as_ref().map_or(1, |i| 4 + i.width() * 2);
            function
                + self
                    .argument_identity
                    .as_ref()
                    .map_or(0, |i| 3 + i.width() * 2)
        }
        #[cfg(not(feature = "incremental"))]
        1
    }
}
impl PartialEq for Call {
    fn eq(&self, other: &Self) -> bool {
        #[cfg(feature = "incremental")]
        match (&self.argument_identity, &other.argument_identity) {
            (Some(a), Some(b)) => {
                if a != b {
                    return false;
                }
            }
            (None, None) => {
                if self.argument != other.argument {
                    return false;
                }
            }
            _ => return false,
        }
        #[cfg(not(feature = "incremental"))]
        if self.argument != other.argument {
            return false;
        }
        #[cfg(feature = "incremental")]
        match (&self.identity, &other.identity) {
            (Some(a), Some(b)) => return a == b,
            (None, None) => {}
            _ => return false,
        }
        self.function == other.function
    }
}
impl Eq for Call {}
impl std::hash::Hash for Call {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        #[cfg(feature = "incremental")]
        match &self.argument_identity {
            Some(identity) => {
                1u8.hash(state);
                identity.hash(state);
            }
            None => {
                0u8.hash(state);
                self.argument.hash(state);
            }
        }
        #[cfg(not(feature = "incremental"))]
        self.argument.hash(state);
        #[cfg(feature = "incremental")]
        if let Some(identity) = &self.identity {
            1u8.hash(state);
            identity.hash(state);
            return;
        }
        0u8.hash(state);
        self.function.hash(state);
    }
}
type CaptureKey = Vec<(LocalId, CellId)>;
#[cfg(feature = "incremental")]
#[derive(Clone, Copy, Default)]
pub(super) struct Access {
    pub epoch: u64,
    pub hits: u8,
}
#[derive(Clone, Copy)]
struct IndexedCell {
    cell: CellId,
    #[cfg(feature = "incremental")]
    access: Access,
}
impl IndexedCell {
    fn new(cell: CellId, _epoch: u64) -> Self {
        Self {
            cell,
            #[cfg(feature = "incremental")]
            access: Access {
                epoch: _epoch,
                hits: 0,
            },
        }
    }
}
#[derive(Default, Clone)]
struct Captures {
    max_width: usize,
    entries: BTreeMap<CaptureKey, IndexedCell>,
}
/// Original call-body activation, separate from execution identity. It is needed
/// only when a reused result's lexical edges require receiving-scope projection.
#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Scope {
    #[cfg(feature = "incremental")]
    body: Option<super::EnvId>,
}
impl Scope {
    fn improves(self, _previous: Self) -> bool {
        #[cfg(feature = "incremental")]
        return self.body.is_some() && _previous.body.is_none();
        #[cfg(not(feature = "incremental"))]
        false
    }
    #[cfg(feature = "incremental")]
    pub(super) fn body(self) -> Option<super::EnvId> {
        self.body
    }
    pub fn new(_body: super::EnvId) -> Self {
        Self {
            #[cfg(feature = "incremental")]
            body: Some(_body),
        }
    }
}
#[derive(Default, Clone, Copy)]
pub(super) struct Completion {
    pub value: ValueId,
    pub cost: cost::Stored,
    pub proof: super::evidence::Proof,
    pub stamp: super::validation::Stamp,
    pub scope: Scope,
    #[cfg(feature = "incremental")]
    pub access: Access,
}
#[derive(Clone)]
enum Indexed {
    Call(Call),
    #[cfg(feature = "incremental")]
    Expression(ExprId),
    #[cfg(feature = "incremental")]
    Capture(ExprId, CaptureKey),
}
impl Indexed {
    fn call(&self) -> Option<&Call> {
        match self {
            Self::Call(key) => Some(key),
            #[cfg(feature = "incremental")]
            Self::Expression(_) | Self::Capture(..) => None,
        }
    }
}
#[cfg(feature = "incremental")]
#[derive(Clone, Copy)]
pub(super) enum Candidate<'a> {
    Call(&'a Call, Completion),
    Expression(ExprId, CellId),
    Capture(ExprId, &'a [(LocalId, CellId)], CellId),
    Global(CellId),
}
// Conservative search prefilter, never result identity or validity evidence.
// Bits survive eviction: collisions/stale bits only cause an unnecessary lookup.
// Rebuilding the bounded memo (transfer/collection) starts with an empty filter.
#[cfg(feature = "incremental")]
#[derive(Clone)]
struct CallFilter([u64; 64]);
#[cfg(feature = "incremental")]
impl Default for CallFilter {
    fn default() -> Self {
        Self([0; 64])
    }
}
#[cfg(feature = "incremental")]
impl CallFilter {
    fn bit(id: usize, semantic: bool) -> (usize, u64) {
        // Mix both high and low ID bits; equality is always checked by Call.
        let mut hash = (id as u64).wrapping_mul(0x9e3779b97f4a7c15);
        hash ^= hash >> 32;
        hash ^= if semantic { 0x517cc1b727220a95 } else { 0 };
        let bit = hash as usize & 4095;
        (bit / 64, 1 << (bit % 64))
    }
    fn contains(&self, id: usize, semantic: bool) -> bool {
        let (word, bit) = Self::bit(id, semantic);
        self.0[word] & bit != 0
    }
    fn insert(&mut self, key: &Call) {
        let (id, semantic) = key
            .identity
            .as_ref()
            .map_or((key.function, false), |s| (s.body().0, true));
        let (word, bit) = Self::bit(id, semantic);
        self.0[word] |= bit;
    }
}
#[derive(Default, Clone)]
pub(super) struct Memo {
    values: HashMap<Call, Completion>,
    order: VecDeque<Indexed>,
    expressions: BTreeMap<ExprId, IndexedCell>,
    captures: HashMap<ExprId, Captures>,
    capture_slots: usize,
    call_slots: usize,
    #[cfg(feature = "incremental")]
    call_filter: CallFilter,
    #[cfg(feature = "incremental")]
    epoch: u64,
}
impl Memo {
    /// A negative answer proves no stored key can match this closure. A positive
    /// answer still requires the complete argument/capture key and all witnesses.
    #[cfg(feature = "incremental")]
    pub(super) fn may_have_call(&self, function: ValueId, body: ExprId) -> bool {
        self.call_filter.contains(function, false) || self.call_filter.contains(body.0, true)
    }

    fn epoch(&self) -> u64 {
        #[cfg(feature = "incremental")]
        {
            self.epoch
        }
        #[cfg(not(feature = "incremental"))]
        {
            0
        }
    }

    #[cfg(feature = "incremental")]
    pub(super) fn advance(&mut self) -> u64 {
        self.epoch = self.epoch.saturating_add(1);
        self.epoch
    }
    /// Only the evaluator may confirm a successful validation of this stored
    /// result. A newly allocated projected value has a different witness and
    /// cannot confirm the original stored result.
    #[cfg(feature = "incremental")]
    pub(super) fn confirm_call(&mut self, key: &Call, stamp: super::validation::Stamp) {
        if let Some(entry) = self.values.get_mut(key) {
            entry.stamp = stamp;
        }
    }
    #[cfg(feature = "incremental")]
    pub(super) fn note_call_hit(&mut self, key: &Call) {
        let epoch = self.advance();
        if let Some(entry) = self.values.get_mut(key) {
            entry.access.epoch = epoch;
            entry.access.hits = entry.access.hits.saturating_add(1).min(3);
        }
    }
    #[cfg(feature = "incremental")]
    pub(super) fn note_expression_hit(&mut self, expression: ExprId) {
        let epoch = self.advance();
        if let Some(entry) = self.expressions.get_mut(&expression) {
            entry.access.epoch = epoch;
            entry.access.hits = entry.access.hits.saturating_add(1).min(3);
        }
    }
    #[cfg(feature = "incremental")]
    pub(super) fn note_capture_hit(&mut self, expression: ExprId, key: &[(LocalId, CellId)]) {
        let epoch = self.advance();
        if let Some(entry) = self
            .captures
            .get_mut(&expression)
            .and_then(|c| c.entries.get_mut(key))
        {
            entry.access.epoch = epoch;
            entry.access.hits = entry.access.hits.saturating_add(1).min(3);
        }
    }
    #[cfg(feature = "incremental")]
    pub(super) fn eviction_work_bound(&self, count: usize) -> u64 {
        self.order.iter().take(count).fold(0u64, |work, entry| {
            let slots = match entry {
                Indexed::Call(key) => key.slots(),
                Indexed::Expression(_) => 1,
                Indexed::Capture(_, key) => key.len().saturating_add(2),
            };
            work.saturating_add(1 + slots as u64)
                .saturating_add(1 + slots as u64)
        })
    }
    #[cfg(feature = "incremental")]
    pub(super) fn eviction_window(
        &self,
        count: usize,
    ) -> impl Iterator<Item = (usize, Candidate<'_>, Access, usize)> {
        self.order
            .iter()
            .take(count)
            .enumerate()
            .map(|(index, entry)| {
                let (candidate, access, slots) = match entry {
                    Indexed::Call(key) => {
                        let (key, done) = self.values.get_key_value(key).expect("indexed call");
                        (Candidate::Call(key, *done), done.access, key.slots())
                    }
                    Indexed::Expression(expr) => {
                        let entry = self.expressions[expr];
                        (Candidate::Expression(*expr, entry.cell), entry.access, 1)
                    }
                    Indexed::Capture(expr, key) => {
                        let entry = self.captures[expr].entries[key];
                        (
                            Candidate::Capture(*expr, key, entry.cell),
                            entry.access,
                            key.len() + 2,
                        )
                    }
                };
                (index, candidate, access, slots)
            })
    }
    #[cfg(feature = "incremental")]
    pub(super) fn candidates(&self) -> impl Iterator<Item = Candidate<'_>> {
        self.order.iter().rev().map(|entry| match entry {
            Indexed::Call(key) => {
                let (key, completion) = self.values.get_key_value(key).expect("indexed call");
                Candidate::Call(key, *completion)
            }
            Indexed::Expression(expr) => Candidate::Expression(*expr, self.expressions[expr].cell),
            Indexed::Capture(expr, key) => {
                Candidate::Capture(*expr, key, self.captures[expr].entries[key].cell)
            }
        })
    }
    #[cfg(all(test, feature = "incremental"))]
    pub(super) fn forget_non_call_indexes(&mut self) {
        self.expressions.clear();
        self.captures.clear();
        self.capture_slots = 0;
        self.order.retain(|entry| matches!(entry, Indexed::Call(_)));
    }
    #[cfg(feature = "incremental")]
    pub(super) fn proofs(&self) -> impl Iterator<Item = super::evidence::Proof> + '_ {
        self.values.values().map(|entry| entry.proof)
    }
    pub(super) fn entries(&self) -> impl Iterator<Item = (Call, ValueId)> + '_ {
        self.order.iter().filter_map(Indexed::call).map(|key| {
            let (stored, completion) = self.values.get_key_value(key).expect("indexed call");
            (stored.clone(), completion.value)
        })
    }
    pub(super) fn expressions(&self) -> impl Iterator<Item = (ExprId, CellId)> + '_ {
        self.expressions.iter().map(|(e, t)| (*e, t.cell))
    }
    pub(super) fn expression(&self, expr: ExprId) -> Option<CellId> {
        self.expressions.get(&expr).map(|entry| entry.cell)
    }
    pub(super) fn remember_expression(&mut self, expr: ExprId, thunk: CellId, capacity: usize) {
        if capacity == 0 || self.expressions.contains_key(&expr) {
            return;
        }
        #[cfg(not(feature = "incremental"))]
        if self.permanent_len() >= capacity {
            return;
        }
        self.make_room(capacity);
        self.expressions
            .insert(expr, IndexedCell::new(thunk, self.epoch()));
        #[cfg(feature = "incremental")]
        self.order.push_back(Indexed::Expression(expr));
    }
    #[cfg(feature = "incremental")]
    pub(super) fn remember_completed_expression(
        &mut self,
        expr: ExprId,
        cell: CellId,
        capacity: usize,
    ) {
        // Refresh an obsolete scope witness without growing the FIFO or changing
        // executable identity. Inspection never calls this mutating admission.
        let epoch = self.epoch;
        if let Some(previous) = self.expressions.get_mut(&expr) {
            previous.cell = cell;
            previous.access = Access { epoch, hits: 0 };
        } else {
            self.remember_expression(expr, cell, capacity);
        }
    }
    pub(super) fn len(&self) -> usize {
        self.call_slots + self.permanent_len()
    }
    pub(super) fn get(
        &self,
        key: &Call,
    ) -> Option<(
        ValueId,
        ValueId,
        CellId,
        super::evidence::Proof,
        super::validation::Stamp,
    )> {
        self.values.get_key_value(key).map(|(stored, entry)| {
            (
                entry.value,
                stored.function,
                stored.argument,
                entry.proof,
                entry.stamp,
            )
        })
    }
    #[cfg(test)]
    pub(super) fn insert(&mut self, key: Call, value: ValueId, capacity: usize) {
        self.insert_cost(key, value, Default::default(), capacity);
    }
    #[cfg(test)]
    pub(super) fn insert_cost(
        &mut self,
        key: Call,
        value: ValueId,
        cost: cost::Stored,
        capacity: usize,
    ) -> bool {
        self.insert_completion(
            key,
            Completion {
                value,
                cost,
                ..Default::default()
            },
            capacity,
        )
    }
    pub(super) fn insert_completion(
        &mut self,
        key: Call,
        #[allow(unused_mut)] mut completion: Completion,
        capacity: usize,
    ) -> bool {
        #[cfg(feature = "incremental")]
        {
            completion.access = Access {
                epoch: self.epoch,
                hits: 0,
            };
        }
        let slots = key.slots();
        if slots > capacity {
            return false;
        }
        #[cfg(not(feature = "incremental"))]
        if self.permanent_len().saturating_add(slots) > capacity {
            return false;
        }
        if let Some(previous) = self.values.get(&key) {
            if previous.stamp == completion.stamp && !completion.scope.improves(previous.scope) {
                return false;
            }
            // Replace the scope witness too; preserve FIFO age and slot charge.
            self.values.remove(&key);
            #[cfg(feature = "incremental")]
            self.call_filter.insert(&key);
            self.values.insert(key, completion);
            return true;
        }
        self.make_room(capacity - slots + 1);
        self.call_slots += slots;
        #[cfg(feature = "incremental")]
        self.call_filter.insert(&key);
        self.values.insert(key.clone(), completion);
        self.order.push_back(Indexed::Call(key));
        true
    }
    #[cfg(feature = "incremental")]
    pub(super) fn body_scope(&self, key: &Call) -> Option<super::EnvId> {
        self.values.get(key)?.scope.body
    }
    fn permanent_len(&self) -> usize {
        self.expressions.len() + self.capture_slots
    }
    pub(super) fn cost(&self, key: &Call) -> cost::Stored {
        self.values
            .get(key)
            .map(|entry| entry.cost)
            .unwrap_or_default()
    }
    #[cfg(feature = "cost-accounting")]
    pub(super) fn costs(&self) -> impl Iterator<Item = cost::WorkCost> + '_ {
        self.values.values().filter_map(|entry| entry.cost.sample())
    }
    pub(super) fn capture_width(&self, expr: ExprId) -> Option<usize> {
        self.captures.get(&expr).map(|c| c.max_width)
    }
    pub(super) fn captured(&self, expr: ExprId, key: &[(LocalId, CellId)]) -> Option<CellId> {
        self.captures
            .get(&expr)?
            .entries
            .get(key)
            .map(|entry| entry.cell)
    }
    pub(super) fn remember_capture(
        &mut self,
        expr: ExprId,
        key: Vec<(LocalId, CellId)>,
        thunk: CellId,
        capacity: usize,
    ) -> bool {
        let slots = key.len().saturating_add(2);
        if slots > capacity || self.captured(expr, &key).is_some() {
            return false;
        }
        #[cfg(not(feature = "incremental"))]
        if self.permanent_len().saturating_add(slots) > capacity {
            return false;
        }
        self.make_room(capacity - slots + 1);
        self.capture_slots += slots;
        let entry = IndexedCell::new(thunk, self.epoch());
        let captures = self.captures.entry(expr).or_default();
        captures.max_width = captures.max_width.max(key.len());
        #[cfg(feature = "incremental")]
        self.order.push_back(Indexed::Capture(expr, key.clone()));
        captures.entries.insert(key, entry);
        true
    }
    fn make_room(&mut self, capacity: usize) {
        while self.len() >= capacity {
            self.evict_at(0);
        }
    }
    pub(super) fn evict_at(&mut self, index: usize) {
        let entry = self.order.remove(index).expect("indexed victim");
        self.remove_indexed(entry);
    }
    #[cfg(feature = "incremental")]
    pub(super) fn retain_candidates(
        &mut self,
        mut keep: impl FnMut(Candidate<'_>) -> bool,
    ) -> usize {
        let mut removed = 0;
        // A linear pass preserves the order without repeated middle removals.
        for _ in 0..self.order.len() {
            let entry = self.order.pop_front().unwrap();
            let candidate = match &entry {
                Indexed::Call(key) => Candidate::Call(key, self.values[key]),
                Indexed::Expression(expr) => {
                    Candidate::Expression(*expr, self.expressions[expr].cell)
                }
                Indexed::Capture(expr, key) => {
                    Candidate::Capture(*expr, key, self.captures[expr].entries[key].cell)
                }
            };
            if keep(candidate) {
                self.order.push_back(entry);
            } else {
                self.remove_indexed(entry);
                removed += 1;
            }
        }
        removed
    }
    fn remove_indexed(&mut self, entry: Indexed) {
        match entry {
            Indexed::Call(key) => {
                self.call_slots -= key.slots();
                self.values.remove(&key);
            }
            #[cfg(feature = "incremental")]
            Indexed::Expression(expr) => {
                self.expressions.remove(&expr);
            }
            #[cfg(feature = "incremental")]
            Indexed::Capture(expr, key) => {
                self.capture_slots -= key.len() + 2;
                let captures = self.captures.get_mut(&expr).expect("indexed capture");
                captures.entries.remove(&key);
                // A conservative width hint remains bounded by previously
                // admitted keys. Avoid scanning sibling keys on every eviction.
                if captures.entries.is_empty() {
                    self.captures.remove(&expr);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "incremental")]
    #[test]
    fn call_filter_never_hides_live_candidates_after_eviction_or_fork() {
        use std::sync::Arc;
        let mut memo = Memo::default();
        assert!(!memo.may_have_call(10, ExprId(20)));
        for n in 0..200 {
            let mut key = Call::allocated(n, CellId(n + 1000));
            if n % 2 == 0 {
                key.identity = Some(Arc::new(super::super::identity::Signature::new(
                    LocalId(0),
                    ExprId(n + 2000),
                    vec![],
                )));
            }
            memo.insert(key, n + 3000, 24);
            let fork = memo.clone();
            for (stored, value) in memo.entries() {
                let mut query = stored.clone();
                let body = if let Some(signature) = &stored.identity {
                    // Equivalent closures may have different allocated handles.
                    query.function += 10_000;
                    signature.body()
                } else {
                    ExprId(999_999)
                };
                assert!(fork.may_have_call(query.function, body));
                assert_eq!(fork.get(&query).unwrap().0, value);
            }
        }
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn call_filter_collisions_never_establish_key_equality() {
        let mut memo = Memo::default();
        let key = Call::allocated(1, CellId(0));
        memo.insert(key.clone(), 99, 8);
        let collision = (2..100_000)
            .find(|&id| CallFilter::bit(id, false) == CallFilter::bit(1, false))
            .expect("finite filter has collisions");
        assert!(memo.may_have_call(collision, ExprId(0)));
        assert!(memo.get(&Call::allocated(collision, CellId(0))).is_none());
        memo.evict_at(0);
        assert!(memo.may_have_call(1, ExprId(0)), "stale bits are allowed");
        assert!(memo.get(&key).is_none());
    }

    #[cfg(not(feature = "incremental"))]
    #[test]
    fn captured_keys_charge_bindings_and_evict_calls_within_the_shared_bound() {
        let mut memo = Memo::default();
        for n in 0..8 {
            memo.insert(Call::allocated(n, CellId(n)), n, 8);
        }
        let key = vec![(LocalId(0), CellId(3)), (LocalId(1), CellId(4))];
        assert!(memo.remember_capture(ExprId(0), key.clone(), CellId(9), 8));
        assert_eq!(memo.len(), 8);
        assert_eq!(memo.entries().count(), 4);
        assert_eq!(memo.captured(ExprId(0), &key), Some(CellId(9)));
        assert!(!memo.remember_capture(ExprId(0), key.clone(), CellId(10), 8));
        memo.remember_expression(ExprId(1), CellId(11), 8);
        assert!(!memo.remember_capture(ExprId(2), key.clone(), CellId(12), 8));
        for n in 10..30 {
            memo.insert(Call::allocated(n, CellId(n)), n, 8);
        }
        assert_eq!(memo.len(), 8);
        assert_eq!(memo.entries().count(), 3);
        assert_eq!(memo.captured(ExprId(0), &key), Some(CellId(9)));
    }

    #[cfg(not(feature = "incremental"))]
    #[test]
    fn call_churn_and_expression_admission_share_one_bound() {
        let mut memo = Memo::default();
        memo.remember_expression(ExprId(0), CellId(10), 4);
        for n in 0..100 {
            memo.insert(Call::allocated(n, CellId(n)), n, 4);
            assert!(memo.len() <= 4);
            assert_eq!(memo.expression(ExprId(0)), Some(CellId(10)));
        }
        assert!(memo.get(&Call::allocated(0, CellId(0))).is_none());
        assert_eq!(
            memo.get(&Call::allocated(99, CellId(99))),
            Some((99, 99, CellId(99), Default::default(), Default::default()))
        );
        for n in 1..10 {
            memo.remember_expression(ExprId(n), CellId(n + 10), 4);
            assert!(memo.len() <= 4);
        }
        assert_eq!(memo.expressions().count(), 4);
        assert_eq!(memo.entries().count(), 0);
        memo.insert(Call::allocated(100, CellId(100)), 100, 4);
        assert_eq!(memo.len(), 4);
        assert!(memo.expression(ExprId(4)).is_none());
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn every_optional_index_kind_can_evict_every_other_kind() {
        let mut memo = Memo::default();
        memo.remember_expression(ExprId(0), CellId(10), 8);
        assert!(memo.remember_capture(ExprId(1), vec![(LocalId(1), CellId(11))], CellId(12), 8));
        for n in 0..20 {
            memo.insert(Call::allocated(n, CellId(n)), n, 8);
        }
        assert_eq!(memo.len(), 8);
        assert_eq!(memo.expressions().count(), 0);
        assert!(memo.captures.is_empty());
        assert_eq!(memo.entries().count(), 8);
        for n in 0..20 {
            memo.remember_expression(ExprId(n), CellId(n), 8);
            assert!(memo.len() <= 8);
        }
        assert_eq!(memo.entries().count(), 0);
        assert_eq!(memo.expressions().count(), 8);
        for n in 0..20 {
            assert!(memo.remember_capture(ExprId(n), vec![(LocalId(0), CellId(n))], CellId(n), 8));
            assert!(memo.len() <= 8);
        }
        assert_eq!(memo.expressions().count(), 0);
        memo.insert(Call::allocated(99, CellId(99)), 99, 8);
        assert!(memo.get(&Call::allocated(99, CellId(99))).is_some());
        assert!(memo.len() <= 8);
        assert!(memo.order.len() <= 8);
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn lazy_argument_proofs_share_the_call_bound_and_leave_on_eviction() {
        use std::sync::Arc;
        let argument = Arc::new(super::super::identity::Argument::new(
            ExprId(1),
            vec![(LocalId(2), CellId(3))],
        ));
        let weak = Arc::downgrade(&argument);
        let mut key = Call::allocated(10, CellId(11));
        key.argument_identity = Some(argument);
        let mut equivalent = key.clone();
        equivalent.argument = CellId(12);
        assert_eq!(key, equivalent);
        assert_ne!(key, Call::allocated(10, CellId(11)));
        let mut memo = Memo::default();
        memo.insert(key, 99, 6);
        assert_eq!(memo.len(), 6);
        assert_eq!(
            memo.get(&equivalent),
            Some((99, 10, CellId(11), Default::default(), Default::default()))
        );
        drop(equivalent);
        memo.insert(Call::allocated(20, CellId(21)), 42, 6);
        assert_eq!(memo.len(), 1);
        assert!(weak.upgrade().is_none());
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn semantic_support_is_bounded_and_evicted_with_its_completed_call() {
        use std::sync::Arc;
        let signature = Arc::new(super::super::identity::Signature::new(
            LocalId(0),
            ExprId(1),
            vec![(LocalId(2), CellId(3))],
        ));
        let weak = Arc::downgrade(&signature);
        let key = Call {
            function: 10,
            argument: CellId(5),
            identity: Some(signature.clone()),
            argument_identity: None,
        };
        let equivalent = Call {
            function: 11,
            argument: CellId(5),
            identity: Some(signature),
            argument_identity: None,
        };
        let mut memo = Memo::default();
        memo.insert(key, 99, 8);
        assert_eq!(memo.len(), 6);
        assert_eq!(
            memo.get(&equivalent),
            Some((99, 10, CellId(5), Default::default(), Default::default()))
        );
        drop(equivalent);
        for n in 20..30 {
            memo.insert(Call::allocated(n, CellId(n)), n, 8);
        }
        assert_eq!(memo.len(), 8);
        assert!(
            weak.upgrade().is_none(),
            "there is no separate immortal identity index"
        );
    }
}

/// Bounded index occupancy; these counts are not retained graph bytes.
#[cfg(feature = "incremental")]
#[derive(Debug, Clone, Copy)]
pub struct CacheIndexStats {
    pub calls: usize,
    pub call_slots: usize,
    pub expressions: usize,
    pub captured_expressions: usize,
    pub capture_slots: usize,
    /// Live support in the shared allocation family, including sibling owners.
    pub identity_support_bytes: usize,
    pub identity_weak_entries: usize,
}
#[cfg(feature = "incremental")]
impl super::Session {
    pub fn cache_index_stats(&self) -> CacheIndexStats {
        CacheIndexStats {
            calls: self.memo.values.len(),
            call_slots: self.memo.call_slots,
            expressions: self.memo.expressions.len(),
            captured_expressions: self.memo.captures.values().map(|c| c.entries.len()).sum(),
            capture_slots: self.memo.capture_slots,
            identity_support_bytes: self.identity_support.bytes(),
            identity_weak_entries: self.identity_support.entries(),
        }
    }
}
