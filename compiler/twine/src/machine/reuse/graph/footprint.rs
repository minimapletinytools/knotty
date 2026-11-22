//! Collection-local node descriptions obtained from the actual graph copier.
//! They describe storage/edges, never validity or retained-result authority.
use super::*;
const EDGES: usize = 128;
const PAYLOAD: usize = 16 * 1024;
pub(in crate::machine::reuse) const CACHE_BYTES: usize = 128 * 1024;
pub(in crate::machine::reuse) const SCRATCH: usize = PAYLOAD + EDGES * 64;
const SLOTS: usize = 512;

struct Description {
    payload: usize,
    host_bytes: usize,
    edges: Vec<Root>,
    proof: Option<evidence::Proof>,
}
impl<const DESCRIBE: bool> Builder<'_, DESCRIBE> {
    pub(super) fn describe_edge(&mut self, edge: Root) -> Option<()> {
        let edges = self.describing.as_mut()?;
        if edges.len() == EDGES || self.new.limits.cancellation.load(Ordering::Relaxed) {
            return None;
        }
        edges.push(edge);
        Some(())
    }
}
fn describe(
    old: &Session,
    new: &Session,
    code: &Code,
    root: Root,
    remaining: &mut u64,
) -> Option<Description> {
    let base = object_bytes() as u64 + EDGES as u64;
    if *remaining < base || new.limits.cancellation.load(Ordering::Relaxed) {
        return None;
    }
    let payload = PAYLOAD.min((*remaining - base) as usize);
    let mut budget = Budget {
        entries: 1,
        bytes: payload,
    };
    let empty = Maps::default();
    let mut b = Builder::<true> {
        old,
        new,
        code,
        existing: &empty,
        maps: Maps::default(),
        pending: vec![],
        thunks: vec![],
        values: vec![],
        envs: vec![],
        entries_left: 1,
        budget: &mut budget,
        bytes: 0,
        host_bytes: 0,
        proofs: vec![],
        recoveries: vec![],
        describing: Some(Vec::new()),
    };
    let work = match root {
        Root::Thunk(t) => {
            b.thunks.push(None);
            Work::Thunk(t.0, 0)
        }
        Root::Value(v) => {
            b.values.push(None);
            Work::Value(v, 0)
        }
        Root::Env(e) => {
            b.envs.push(None);
            Work::Env(e, 0)
        }
    };
    let complete = b.copy(work).is_some();
    let edges = b.describing.take().unwrap();
    *remaining -= object_bytes() as u64 + b.bytes as u64 + edges.len() as u64;
    complete.then_some(Description {
        payload: b.bytes,
        host_bytes: b.host_bytes,
        edges,
        proof: b.proofs.first().map(|(_, proof)| *proof),
    })
}

pub(in crate::machine::reuse) struct Probe {
    pub bytes: usize,
    pub complete: bool,
    pub proofs: Vec<evidence::Proof>,
}
/// Borrow the immutable source and mandatory-root map throughout discovery.
/// The cache is dropped before admission changes the destination/maps.
pub(in crate::machine::reuse) struct Cache<'a> {
    old: &'a Session,
    code: &'a Code,
    existing: &'a Maps,
    destination: (u64, usize, usize, usize, usize),
    nodes: Vec<Option<(Root, Description)>>,
    uncached: Option<Description>,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}
impl<'a> Cache<'a> {
    pub fn new(old: &'a Session, new: &Session, code: &'a Code, existing: &'a Maps) -> Self {
        assert!(code.collecting());
        Self {
            old,
            code,
            existing,
            destination: Self::destination(new),
            nodes: Vec::new(),
            uncached: None,
            bytes: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }
    fn destination(new: &Session) -> (u64, usize, usize, usize, usize) {
        (
            new.id,
            new.thunks.len(),
            new.values.len(),
            new.envs.len(),
            new.stats.host_bytes,
        )
    }
    fn slot(root: Root) -> usize {
        let (id, tag): (usize, usize) = match root {
            Root::Value(v) => (v, 0),
            Root::Thunk(t) => (t.0, 1),
            Root::Env(e) => (e, 2),
        };
        let mixed = id.wrapping_mul(0x9e3779b1) ^ tag.wrapping_mul(0x85ebca6b);
        (mixed ^ (mixed >> 16)) & (SLOTS - 1)
    }
    fn node(&mut self, new: &Session, root: Root, remaining: &mut u64) -> Option<&Description> {
        if *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        let slot = Self::slot(root);
        if self
            .nodes
            .get(slot)
            .and_then(Option::as_ref)
            .is_some_and(|(key, _)| *key == root)
        {
            self.hits += 1;
            return self.nodes[slot].as_ref().map(|(_, node)| node);
        }
        self.misses += 1;
        self.uncached = None;
        let description = describe(self.old, new, self.code, root, remaining)?;
        if self.nodes.is_empty() {
            let bytes = SLOTS * std::mem::size_of::<Option<(Root, Description)>>();
            let work = bytes as u64 + SLOTS as u64;
            if bytes <= CACHE_BYTES && work <= *remaining {
                *remaining -= work;
                self.nodes = Vec::with_capacity(SLOTS);
                self.nodes.resize_with(SLOTS, || None);
                self.bytes =
                    self.nodes.capacity() * std::mem::size_of::<Option<(Root, Description)>>();
            }
        }
        let bytes = description.edges.capacity() * std::mem::size_of::<Root>();
        let previous = self
            .nodes
            .get(slot)
            .and_then(Option::as_ref)
            .map_or(0, |(_, node)| {
                node.edges.capacity() * std::mem::size_of::<Root>()
            });
        let total = self.bytes - previous + bytes;
        if !self.nodes.is_empty()
            && total <= CACHE_BYTES
            && (bytes as u64) < *remaining
            && !new.limits.cancellation.load(Ordering::Relaxed)
        {
            *remaining -= bytes as u64 + 1;
            self.evictions += u64::from(self.nodes[slot].is_some());
            self.nodes[slot] = Some((root, description));
            self.bytes = total;
            self.nodes[slot].as_ref().map(|(_, node)| node)
        } else {
            self.uncached = Some(description);
            self.uncached.as_ref()
        }
    }
    pub fn probe(&mut self, new: &Session, roots: &[Root], remaining: &mut u64) -> Probe {
        assert_eq!(self.destination, Self::destination(new));
        assert!(Arc::ptr_eq(&self.old.program, &new.program));
        assert!(Arc::ptr_eq(&self.old.hosts, &new.hosts));
        let mut result = Probe {
            bytes: 0,
            complete: true,
            proofs: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        let mut pending = Vec::new();
        let mut host_bytes = 0usize;
        let max_entries = 64.min(new.limits.heap_entries.saturating_sub(new.heap_entries()));
        let existing = self.existing;
        let enqueue = |root,
                       seen: &mut std::collections::HashSet<Root>,
                       pending: &mut Vec<Root>,
                       bytes: &mut usize,
                       fuel: &mut u64| {
            if *fuel == 0 {
                return None;
            }
            *fuel -= 1;
            let retained = match root {
                Root::Thunk(t) => existing.thunks.contains_key(&t.0),
                Root::Value(v) => existing.values.contains_key(&v),
                Root::Env(e) => existing.envs.contains_key(&e),
            };
            if retained || seen.contains(&root) {
                return Some(());
            }
            if seen.len() >= max_entries || object_bytes() > PAYLOAD.saturating_sub(*bytes) {
                return None;
            }
            *bytes += object_bytes();
            seen.insert(root);
            pending.push(root);
            Some(())
        };
        let done = (|| {
            for root in roots {
                enqueue(*root, &mut seen, &mut pending, &mut result.bytes, remaining)?;
            }
            while let Some(root) = pending.pop() {
                if new.limits.cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                let description = self.node(new, root, remaining)?;
                if description.payload > PAYLOAD.saturating_sub(result.bytes) {
                    return None;
                }
                result.bytes += description.payload;
                host_bytes = host_bytes.checked_add(description.host_bytes)?;
                if host_bytes > new.limits.host_bytes.saturating_sub(new.stats.host_bytes) {
                    return None;
                }
                if let Some(proof) = description.proof {
                    result.proofs.push(proof);
                }
                for edge in &description.edges {
                    enqueue(*edge, &mut seen, &mut pending, &mut result.bytes, remaining)?;
                }
            }
            Some(())
        })()
        .is_some();
        result.complete = done;
        if !done {
            result.proofs.clear();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(n: &str) -> GlobalName {
        GlobalName {
            module: vec!["Test".into()],
            name: n.into(),
        }
    }
    fn session() -> Session {
        let ast = knot_syntax::parse("module Test exposing (..)\nmake :: Int -> Int -> Int -> Int\nmake ignored kept later = kept\nbox = make (error \"unused\") 7\nones :: List Int\nones = 1 : ones\npair = (box, ones)\nanswer = box 3\nfailure :: Int\nfailure = error \"kept error\"\n").unwrap();
        let canonical = knot_canonical::canonicalize_module(&ast, None).unwrap();
        let program = knot_core::lower(&[
            knot_checker::check::check_library_program("Prelude").unwrap(),
            knot_checker::check::check_program(&canonical).unwrap(),
        ]);
        Session::new(
            Arc::new(program),
            Limits {
                compact_evidence: true,
                region_evidence: true,
                joint_retention: true,
                ..Default::default()
            },
        )
    }
    #[test]
    fn shared_descriptions_match_real_collection_for_lazy_cycles_scopes_and_errors() {
        let mut old = session();
        for binding in ["box", "ones", "pair", "answer"] {
            old.evaluate(&name(binding)).unwrap();
        }
        assert!(old.evaluate(&name("failure")).is_err());
        let mut new = Session::empty(old.program.clone(), old.limits.clone(), old.hosts.clone());
        let code = Code::collection(&old, &new, 1_000_000).unwrap();
        let mut maps = Maps::default();
        maps.envs.insert(0, 0);
        let old_heap = old.heap_entries();
        let old_work = old.stats.steps;
        let mut cache = Cache::new(&old, &new, &code, &maps);
        for binding in ["box", "ones", "pair", "answer", "failure"] {
            let root = Root::Thunk(old.globals[&Global::Value(name(binding))]);
            let mut budget = Budget {
                entries: 64,
                bytes: PAYLOAD,
            };
            let staged = transfer(&old, &new, &code, &maps, root, &mut budget).unwrap();
            let mut fuel = 1_000_000;
            let estimate = cache.probe(&new, &[root], &mut fuel);
            assert!(estimate.complete, "{binding}");
            assert_eq!(estimate.bytes, staged.bytes, "{binding}");
            assert_eq!(
                estimate.proofs,
                staged.proofs.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
                "{binding}"
            );
            let mut again_fuel = 1_000_000;
            let again = cache.probe(&new, &[root], &mut again_fuel);
            assert_eq!(again.bytes, estimate.bytes, "priced is not retained");
            assert!(again.complete);
            assert!(
                again_fuel >= fuel,
                "repeated probes do not rediscover nodes"
            );
        }
        assert!(cache.hits > 0);
        assert!(cache.bytes <= CACHE_BYTES);
        assert_eq!(old.heap_entries(), old_heap);
        assert_eq!(old.stats.steps, old_work);
        assert_eq!(new.evidence_stats().certificates, 0);
        drop(cache);
        let root = Root::Thunk(old.globals[&Global::Value(name("answer"))]);
        transfer(
            &old,
            &new,
            &code,
            &maps,
            root,
            &mut Budget {
                entries: 64,
                bytes: PAYLOAD,
            },
        )
        .unwrap()
        .commit_with_proofs(&mut new, &mut maps, &old, &code);
        let mut cache = Cache::new(&old, &new, &code, &maps);
        let covered = cache.probe(&new, &[root], &mut 1000);
        assert!(covered.complete);
        assert_eq!(
            covered.bytes, 0,
            "only real retained roots remove graph storage"
        );
    }
    #[test]
    fn incomplete_wide_nodes_and_small_or_cancelled_budgets_never_poison_descriptions() {
        let mut old = session();
        let leaf = old.globals[&Global::Value(name("answer"))];
        let wide = old.values.len();
        old.values.push(Value::Tuple(vec![leaf; EDGES + 1]));
        let huge = old.values.len();
        old.values
            .push(Value::Lit(Literal::String("x".repeat(PAYLOAD + 1))));
        let new = Session::empty(old.program.clone(), old.limits.clone(), old.hosts.clone());
        let code = Code::collection(&old, &new, 1_000_000).unwrap();
        let maps = Maps::default();
        let mut cache = Cache::new(&old, &new, &code, &maps);
        for root in [Root::Value(wide), Root::Value(huge)] {
            let estimate = cache.probe(&new, &[root], &mut 1_000_000);
            assert!(!estimate.complete);
            assert!(estimate.proofs.is_empty());
            assert!(!cache.nodes.iter().flatten().any(|(key, _)| *key == root));
        }
        for initial in [0, 1, 100, 1000] {
            let mut fuel = initial;
            assert!(!cache.probe(&new, &[Root::Value(wide)], &mut fuel).complete);
            assert!(fuel <= initial);
        }
        new.limits.cancellation.store(true, Ordering::Relaxed);
        assert!(!cache.probe(&new, &[Root::Value(wide)], &mut 1000).complete);
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn description_storage_is_bounded_even_when_many_distinct_nodes_are_probed() {
        let mut old = session();
        let start = old.values.len();
        for n in 0..1000 {
            old.values.push(Value::Lit(Literal::String("x".repeat(n))));
        }
        let new = Session::empty(old.program.clone(), old.limits.clone(), old.hosts.clone());
        let code = Code::collection(&old, &new, 1_000_000).unwrap();
        let maps = Maps::default();
        let mut cache = Cache::new(&old, &new, &code, &maps);
        for value in start..start + 1000 {
            assert!(
                cache
                    .probe(&new, &[Root::Value(value)], &mut 100_000)
                    .complete
            );
            assert!(cache.nodes.len() <= 512);
            assert!(cache.bytes <= CACHE_BYTES);
        }
        assert_eq!(cache.nodes.len(), 512);
        assert!(cache.evictions > 0);
        let last = Root::Value(start + 999);
        let collision = (start..start + 999)
            .map(Root::Value)
            .find(|root| Cache::slot(*root) == Cache::slot(last))
            .unwrap();
        let mut warm = 100_000;
        let mut cold = 100_000;
        let warm_result = cache.probe(&new, &[last], &mut warm);
        let cold_result = cache.probe(&new, &[collision], &mut cold);
        assert!(warm > cold);
        assert_ne!(
            warm_result.bytes, cold_result.bytes,
            "a slot collision cannot alias node descriptions"
        );
        for root in [collision, last, collision] {
            let actual = transfer(
                &old,
                &new,
                &code,
                &maps,
                root,
                &mut Budget {
                    entries: 64,
                    bytes: PAYLOAD,
                },
            )
            .unwrap();
            let priced = cache.probe(&new, &[root], &mut 100_000);
            assert!(priced.complete);
            assert_eq!(priced.bytes, actual.bytes);
        }
    }
}
