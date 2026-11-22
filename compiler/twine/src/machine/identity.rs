//! Exact, session-local computation descriptions. Support describes recipes,
//! never evaluated values. Keys own a bounded DAG; the lookup is weak.
//! Revision validation and inspection provenance remain separate checks.
use super::*;
use std::hash::{Hash, Hasher};

const SUPPORT_NODES: usize = 128;
const SUPPORT_DEPTH: usize = 32;
// Bound expanded equality work as well as allocated DAG nodes. A diamond can
// have few nodes but exponentially many unfolded paths.
const SUPPORT_SLOTS: usize = 1024;
#[derive(Debug)]
struct Input {
    cell: CellId,
    recipe: Option<Arc<Argument>>,
}
impl Input {
    #[cfg(test)]
    fn allocated(cell: CellId) -> Self {
        Self { cell, recipe: None }
    }
    fn slots(&self) -> usize {
        self.recipe.as_ref().map_or(0, |r| r.slots)
    }
    fn depth(&self) -> usize {
        self.recipe.as_ref().map_or(0, |r| r.depth)
    }
}
impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        match (&self.recipe, &other.recipe) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
            (None, None) => self.cell == other.cell,
            _ => false,
        }
    }
}
impl Eq for Input {}
impl Hash for Input {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.recipe {
            Some(recipe) => {
                1u8.hash(state);
                recipe.hash(state);
            }
            None => {
                0u8.hash(state);
                self.cell.hash(state);
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct Signature {
    parameter: LocalId,
    body: ExprId,
    inputs: Box<[(LocalId, Input)]>,
    hash: u64,
    slots: usize,
    _charge: Option<identity_support::Charge>,
}
impl Signature {
    pub(super) fn body(&self) -> ExprId {
        self.body
    }

    #[cfg(test)]
    pub fn new(parameter: LocalId, body: ExprId, inputs: Vec<(LocalId, CellId)>) -> Self {
        Self::described(
            parameter,
            body,
            inputs
                .into_iter()
                .map(|(l, c)| (l, Input::allocated(c)))
                .collect(),
            None,
        )
    }
    fn described(
        parameter: LocalId,
        body: ExprId,
        inputs: Vec<(LocalId, Input)>,
        charge: Option<identity_support::Charge>,
    ) -> Self {
        let mut state = std::collections::hash_map::DefaultHasher::new();
        (parameter, body, &inputs).hash(&mut state);
        let slots = inputs
            .iter()
            .fold(4usize.saturating_add(inputs.len() * 2), |n, (_, i)| {
                n.saturating_add(i.slots())
            });
        Self {
            parameter,
            body,
            inputs: inputs.into(),
            hash: state.finish(),
            slots,
            _charge: charge,
        }
    }
    pub fn width(&self) -> usize {
        self.inputs.len()
    }
    pub fn input_cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.inputs.iter().map(|(_, input)| input.cell)
    }
}
impl PartialEq for Signature {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.parameter == other.parameter
            && self.body == other.body
            && self.inputs == other.inputs
    }
}
impl Eq for Signature {}
impl Hash for Signature {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

/// A lazy computation's expression and free-input descriptions. Unknown and
/// introduced inputs stay opaque; recursive/budget-limited edges remain cells.
#[derive(Debug)]
pub(super) struct Argument {
    expression: ExprId,
    inputs: Box<[(LocalId, Input)]>,
    hash: u64,
    slots: usize,
    depth: usize,
    _charge: Option<identity_support::Charge>,
}
impl Argument {
    #[cfg(test)]
    pub fn new(expression: ExprId, inputs: Vec<(LocalId, CellId)>) -> Self {
        Self::described(
            expression,
            inputs
                .into_iter()
                .map(|(l, c)| (l, Input::allocated(c)))
                .collect(),
            None,
        )
    }
    fn described(
        expression: ExprId,
        inputs: Vec<(LocalId, Input)>,
        charge: Option<identity_support::Charge>,
    ) -> Self {
        let mut state = std::collections::hash_map::DefaultHasher::new();
        (expression, &inputs).hash(&mut state);
        let slots = inputs
            .iter()
            .fold(3usize.saturating_add(inputs.len() * 2), |n, (_, i)| {
                n.saturating_add(i.slots())
            });
        let depth = 1 + inputs.iter().map(|(_, i)| i.depth()).max().unwrap_or(0);
        Self {
            expression,
            inputs: inputs.into(),
            hash: state.finish(),
            slots,
            depth,
            _charge: charge,
        }
    }
    pub fn width(&self) -> usize {
        self.inputs.len()
    }
    pub fn expression(&self) -> ExprId {
        self.expression
    }
    pub fn input_cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.inputs.iter().map(|(_, input)| input.cell)
    }
}
impl PartialEq for Argument {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.expression == other.expression
            && self.inputs == other.inputs
    }
}
impl Eq for Argument {}
impl Hash for Argument {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl Session {
    pub(super) fn signature(
        &mut self,
        parameter: LocalId,
        body: ExprId,
        inputs: &[(LocalId, ThunkId)],
    ) -> Option<Arc<Signature>> {
        let bytes = std::mem::size_of::<Signature>()
            .saturating_add(64)
            .saturating_add(
                inputs
                    .len()
                    .saturating_mul(std::mem::size_of::<(LocalId, Input)>()),
            );
        let charge = self.identity_support.reserve(bytes)?;
        let mut remaining = SUPPORT_NODES;
        let mut active = std::collections::HashSet::new();
        let inputs = inputs
            .iter()
            .map(|(local, thunk)| {
                let cell = self.cell_id(*thunk);
                let recipe = self.support(cell, 0, &mut remaining, &mut active);
                (*local, Input { cell, recipe })
            })
            .collect();
        let signature = Signature::described(parameter, body, inputs, Some(charge));
        (signature.slots <= SUPPORT_SLOTS).then(|| Arc::new(signature))
    }

    pub(super) fn argument_recipe(&self, cell: CellId) -> Option<(ExprId, EnvId)> {
        let thunk = &self.thunks[cell.0];
        match thunk.state {
            ThunkState::Pending(Pending::Expr(expr, env)) => Some((expr, env)),
            ThunkState::Done(Ok(Completed {
                expression: Some(expr),
                ..
            })) => {
                let env = usize::try_from(thunk.activation.0).ok()?;
                (self.envs.get(env)?.activation == thunk.activation).then_some((expr, env))
            }
            _ => None,
        }
    }
    fn support(
        &mut self,
        cell: CellId,
        depth: usize,
        remaining: &mut usize,
        active: &mut std::collections::HashSet<CellId>,
    ) -> Option<Arc<Argument>> {
        if *remaining == 0
            || depth >= SUPPORT_DEPTH
            || active.contains(&cell)
            || self.limits.cancellation.load(Ordering::Relaxed)
        {
            return None;
        }
        if self.stats.steps >= self.limits.steps {
            return None;
        }
        self.stats.steps += 1;
        *remaining -= 1;
        if let Some(recipe) = self.identity_support.lookup(cell) {
            if recipe.depth + depth <= SUPPORT_DEPTH {
                return Some(recipe);
            }
            return None;
        }
        let (expression, env) = self.argument_recipe(cell)?;
        let captures = self.capture_inputs(expression, None, env)?;
        let bytes = std::mem::size_of::<Argument>()
            .saturating_add(64)
            .saturating_add(
                captures
                    .len()
                    .saturating_mul(std::mem::size_of::<(LocalId, Input)>()),
            );
        let charge = self.identity_support.reserve(bytes)?;
        active.insert(cell);
        let inputs = captures
            .iter()
            .map(|(local, thunk)| {
                let cell = self.cell_id(*thunk);
                let recipe = self.support(cell, depth + 1, remaining, active);
                (*local, Input { cell, recipe })
            })
            .collect();
        active.remove(&cell);
        let recipe = Arc::new(Argument::described(expression, inputs, Some(charge)));
        if recipe.slots > SUPPORT_SLOTS {
            return None;
        }
        self.identity_support.remember(cell, &recipe);
        Some(recipe)
    }
    pub(super) fn argument_identity_eligible(&self, expr: ExprId) -> bool {
        !matches!(
            self.program.expressions[expr.0].kind,
            Kind::Lit(_)
                | Kind::Rational(_)
                | Kind::Local(_)
                | Kind::Global(_)
                | Kind::Prim(_)
                | Kind::Host(..)
                | Kind::Ctor(..)
                | Kind::Unsupported(_)
        )
    }
    pub(super) fn argument_identity(&mut self, cell: CellId) -> Option<Arc<Argument>> {
        let (expr, _) = self.argument_recipe(cell)?;
        if !self.argument_identity_eligible(expr) {
            return None;
        }
        let mut remaining = SUPPORT_NODES;
        self.support(cell, 0, &mut remaining, &mut Default::default())
    }
    pub(super) fn argument_scope_compatible(&mut self, a: CellId, b: CellId) -> bool {
        if !self.checked_source_scope(self.thunks[a.0].activation.0 as usize)
            || !self.checked_source_scope(self.thunks[b.0].activation.0 as usize)
        {
            return false;
        }
        if a == b {
            return true;
        }
        if self.thunks[a.0].origin != self.thunks[b.0].origin {
            return false;
        }
        match (self.argument_recipe(a), self.argument_recipe(b)) {
            (Some((ae, a)), Some((be, b))) if ae == be => self.scopes_compatible(a, b),
            _ => false,
        }
    }

    /// Same computation does not by itself authorize returning old source uses.
    /// Prove the complete lexical graph, including unused/shadowed bindings and
    /// source-use origins. Recreated cells need exact recipe/scope correspondence;
    /// equal values or free-input sets alone are insufficient. This same-program
    /// proof does not project scopes across source edits.
    pub(super) fn call_scope_compatible(&mut self, function: ValueId, witness: ValueId) -> bool {
        let (Value::Closure(_, _, a), Value::Closure(_, _, b)) =
            (&self.values[function], &self.values[witness])
        else {
            return false;
        };
        self.scopes_compatible(a.scope, b.scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(source: &str, bytes: usize) -> Session {
        use knot_checker::check::{check_library_program, check_program};
        let ast = knot_syntax::parse(&format!("module Test exposing (..)\n{source}")).unwrap();
        let canonical = knot_canonical::canonicalize_module(&ast, None).unwrap();
        let program = knot_core::lower(&[
            check_library_program("Prelude").unwrap(),
            check_program(&canonical).unwrap(),
        ]);
        Session::new(
            Arc::new(program),
            Limits {
                dependency_bytes: bytes,
                application_cache_entries: 0,
                ..Default::default()
            },
        )
    }
    fn name(binding: &str) -> GlobalName {
        GlobalName {
            module: vec!["Test".into()],
            name: binding.into(),
        }
    }
    fn closure(s: &mut Session, binding: &str) -> (ValueId, Arc<Signature>) {
        let value = s.evaluate(&name(binding)).unwrap().value;
        let identity = s
            .cache_work(|s| s.function_identity(value))
            .expect("bounded identity");
        (value, identity)
    }
    #[test]
    fn unindexed_cheap_calls_defer_identity_but_later_admitted_calls_still_reuse() {
        let mut s = session(
            "f :: Int -> Int\nf x = x\ninput :: Int\ninput = 42\na = f input\nb = f input\nc = f input\n",
            1_000_000,
        );
        s.limits.application_cache_entries = 1024;
        s.limits.cache_minimum_work = u64::MAX;
        let function = s.evaluate(&name("f")).unwrap().value;
        s.evaluate(&name("a")).unwrap();
        let Value::Closure(_, _, capture) = &s.values[function] else {
            panic!("closure")
        };
        assert!(capture.identity.is_none());
        assert!(s.stats.application_key_skips > 0);
        assert_eq!(s.cache_index_stats().calls, 0);

        s.limits.cache_minimum_work = 0;
        s.evaluate(&name("b")).unwrap();
        let hits = s.stats.application_cache_hits;
        s.evaluate(&name("c")).unwrap();
        assert!(s.stats.application_cache_hits > hits);
    }

    #[test]
    fn reading_an_existing_call_key_keeps_the_forked_value_page_shared() {
        let mut original = session(
            "input :: Int\ninput = 10\nf :: Int -> Int\nf x = x + input\n",
            1_000_000,
        );
        let function = original.evaluate(&name("f")).unwrap().value;
        let argument = original.cell_id(original.globals[&Global::Value(name("input"))]);
        let expected = original.call_key(function, argument);
        let mut fork = original
            .try_fork(original.limits.clone())
            .unwrap()
            .fork
            .unwrap()
            .session;
        assert!(std::ptr::eq(
            &original.values[function],
            &fork.values[function]
        ));
        assert_eq!(fork.call_key(function, argument), expected);
        assert!(
            std::ptr::eq(&original.values[function], &fork.values[function]),
            "reading a cached immutable identity must not detach its value page"
        );
    }
    #[test]
    fn recreated_intermediates_have_equal_support_but_still_need_full_scope_proof() {
        let mut s = session("input :: Int\ninput = 10\nmake :: Int -> Int -> Int\nmake ignored = let middle = input + 1 in \\x -> middle + x\na = make 1\nb = make 2\n", 1_000_000);
        let (a, first) = closure(&mut s, "a");
        let (b, second) = closure(&mut s, "b");
        assert_ne!(a, b);
        assert_eq!(first, second);
        assert_ne!(
            first.input_cells().collect::<Vec<_>>(),
            second.input_cells().collect::<Vec<_>>()
        );
        assert!(
            !s.call_scope_compatible(a, b),
            "unused different bindings must remain visible to the inspector"
        );
        assert!(s.identity_support.bytes() > 0);
        let observer = s.identity_support.fork(0);
        drop(s);
        assert!(observer.bytes() > 0, "keys still own their support");
        drop(first);
        drop(second);
        assert_eq!(
            observer.bytes(),
            0,
            "weak lookup does not pin support or result heaps"
        );
    }
    #[test]
    fn complete_recreated_scope_graphs_authorize_reuse_and_keep_inspection_bindings() {
        let mut s = session("input :: Int\ninput = 10\nmake :: Int -> Int -> Int\nmake seed = let middle = seed + 1 in \\x -> middle + x\na = make input\nb = make input\nc = a input\nd = b input\n", 1_000_000);
        let (a, first) = closure(&mut s, "a");
        let (b, second) = closure(&mut s, "b");
        assert_ne!(a, b);
        assert_eq!(first, second);
        assert!(s.call_scope_compatible(a, b));
        s.limits.application_cache_entries = 16_384;
        s.evaluate(&name("c")).unwrap();
        let hits = s.stats.application_cache_hits;
        let result = s.evaluate(&name("d")).unwrap();
        assert_eq!(
            s.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(21))
        );
        assert!(s.stats.application_cache_hits > hits);
        let Value::Closure(_, _, capture) = &s.values[b] else {
            unreachable!()
        };
        let bindings = s.activation_bindings(s.envs[capture.scope].activation);
        let middle = bindings.iter().find(|b| b.name == "middle").unwrap().handle;
        let value = s.force(middle).unwrap();
        assert_eq!(
            s.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(11))
        );
    }
    #[test]
    fn cached_returned_closure_projects_unused_bindings_to_the_receiving_call() {
        let mut s = session("input :: Int\ninput = 3\nmake :: Int -> Int -> Int -> Int\nmake unused x y = x + y\nf = make 10\ng = make 20\nfirst = f input\nsecond = g input\nanswer = second 4\n", 1_000_000);
        s.limits.application_cache_entries = 16_384;
        let first = s.evaluate(&name("first")).unwrap();
        let before = s.stats.application_projected_hits;
        let second = s.evaluate(&name("second")).unwrap();
        assert!(s.stats.application_projected_hits > before);
        assert_ne!(
            first.value, second.value,
            "closure scopes must be projected, not discarded"
        );
        let root = s.global_handle(&name("second")).unwrap();
        let reads = s.dependency_reads(root, 0, 100).unwrap().unwrap();
        let call = reads
            .iter()
            .find(|read| matches!(read.source, DependencySource::Call { .. }))
            .unwrap();
        assert_eq!(
            call.value.unwrap().value,
            second.value,
            "direct read names the receiving result"
        );
        let DependencySource::Call { function, .. } = call.source else {
            unreachable!()
        };
        let current_function = s.evaluate(&name("g")).unwrap();
        assert_eq!(function.value, current_function.value);
        for (handle, expected) in [(first, 10), (second, 20)] {
            let Value::Closure(_, _, capture) = &s.values[handle.value] else {
                panic!("closure")
            };
            let bindings = s.activation_bindings(s.envs[capture.scope].activation);
            let unused = bindings.iter().find(|b| b.name == "unused").unwrap().handle;
            let value = s.force(unused).unwrap();
            assert_eq!(
                s.view(value).unwrap(),
                ValueView::Literal(&Literal::Int(expected))
            );
        }
        let result = s.evaluate(&name("answer")).unwrap();
        assert_eq!(
            s.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(7))
        );
    }
    #[test]
    fn borrowed_argument_closures_keep_the_receiving_unused_bindings() {
        let mut s = session("input :: Int\ninput = 3\nidentity x = x\nmake :: Int -> Int -> Int\nmake unused = identity (\\y -> input + y)\nfirst = make 10\nsecond = make 20\n", 1_000_000);
        s.limits.application_cache_entries = 16_384;
        s.evaluate(&name("first")).unwrap();
        let result = s.evaluate(&name("second")).unwrap();
        let Value::Closure(_, _, capture) = &s.values[result.value] else {
            panic!("closure")
        };
        let unused = s
            .activation_bindings(s.envs[capture.scope].activation)
            .into_iter()
            .find(|b| b.name == "unused")
            .unwrap()
            .handle;
        let value = s.force(unused).unwrap();
        assert_eq!(
            s.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(20))
        );
    }
    #[test]
    fn projected_scopes_roll_back_and_reused_allocation_slots_do_not_reuse_old_support() {
        let source = "input :: Int\ninput = 3\nmake :: Int -> Int -> Int -> Int\nmake unused x y = x + y\nf = make 10\ng = make 20\nfirst = f input\nsecond = g input\n";
        let mut interrupted = 0;
        for steps in [5, 20, 50, 100, 300] {
            let mut s = session(source, 1_000_000);
            s.limits.application_cache_entries = 16_384;
            s.identity_support = identity_support::Store::new(1_000_000, 16_384);
            s.evaluate(&name("first")).unwrap();
            s.evaluate(&name("g")).unwrap();
            let before = (s.thunks.len(), s.values.len(), s.envs.len());
            let second = s.global_handle(&name("second")).unwrap();
            let inspected = s
                .inspect(
                    second,
                    InspectionBudget {
                        steps,
                        ..Default::default()
                    },
                )
                .unwrap();
            if matches!(inspected, Inspection::Truncated) {
                interrupted += 1;
                assert_eq!((s.thunks.len(), s.values.len(), s.envs.len()), before);
            }
            let result = s.force(second).unwrap();
            let Value::Closure(_, _, capture) = &s.values[result.value] else {
                panic!("closure")
            };
            let unused = s
                .activation_bindings(s.envs[capture.scope].activation)
                .into_iter()
                .find(|b| b.name == "unused")
                .unwrap()
                .handle;
            let value = s.force(unused).unwrap();
            assert_eq!(
                s.view(value).unwrap(),
                ValueView::Literal(&Literal::Int(20))
            );
        }
        assert!(interrupted > 0);
    }
    #[test]
    fn projected_recursive_closures_preserve_cycles_and_current_unused_bindings() {
        let mut s = session("input :: Int\ninput = 3\nmake :: Int -> Int -> Int -> Int\nmake unused x = let loop y = if y == 0 then x else loop (y - 1) in loop\nf = make 10\ng = make 20\nfirst = f input\nsecond = g input\nanswer = second 8\n", 1_000_000);
        s.limits.application_cache_entries = 16_384;
        s.evaluate(&name("first")).unwrap();
        let before = s.stats.application_projected_hits;
        let result = s.evaluate(&name("second")).unwrap();
        assert!(s.stats.application_projected_hits > before);
        let Value::Closure(_, _, capture) = &s.values[result.value] else {
            panic!("closure")
        };
        let unused = s
            .activation_bindings(s.envs[capture.scope].activation)
            .into_iter()
            .find(|b| b.name == "unused")
            .unwrap()
            .handle;
        let value = s.force(unused).unwrap();
        assert_eq!(
            s.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(20))
        );
        let value = s.evaluate(&name("answer")).unwrap();
        assert_eq!(s.view(value).unwrap(), ValueView::Literal(&Literal::Int(3)));
    }
    #[test]
    fn distinct_captured_recipes_and_recursive_cells_do_not_become_equal_values() {
        let mut s = session("make :: Int -> Int -> Int\nmake seed = let middle = seed + 1 in \\x -> middle + x\na = make 1\nb = make 2\n", 1_000_000);
        let (_, first) = closure(&mut s, "a");
        let (_, second) = closure(&mut s, "b");
        assert_ne!(first, second);
        let mut recursive = session("make :: Int -> Int -> Int\nmake seed = let loop = loop in \\x -> if x == 0 then seed else loop\na = make 1\nb = make 2\n", 1_000_000);
        let (_, first) = closure(&mut recursive, "a");
        let (_, second) = closure(&mut recursive, "b");
        assert_ne!(first, second);
        assert!(first.slots <= SUPPORT_SLOTS && second.slots <= SUPPORT_SLOTS);
    }
    #[test]
    fn support_exhaustion_falls_back_without_changing_evaluation() {
        let mut s = session("make :: Int -> Int -> Int\nmake seed = let middle = seed + 1 in \\x -> middle + x\na = make 10 20\n", 0);
        let result = s.evaluate(&name("a")).unwrap();
        assert_eq!(
            s.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(31))
        );
        assert_eq!(s.identity_support.bytes(), 0);
    }
    // The mutable accounting counter is deliberately excluded from key equality/hash.
    #[allow(clippy::mutable_key_type)]
    #[test]
    fn argument_hash_collisions_do_not_equate_recipes_or_input_cells() {
        let a = Argument::new(ExprId(1), vec![(LocalId(2), CellId(3))]);
        let mut b = Argument::new(ExprId(1), vec![(LocalId(2), CellId(4))]);
        b.hash = a.hash;
        assert_ne!(a, b);
        let mut map = std::collections::HashMap::new();
        map.insert(a, "first");
        map.insert(b, "second");
        assert_eq!(map.len(), 2);
    }
    #[allow(clippy::mutable_key_type)]
    #[test]
    fn hashes_only_narrow_candidates_and_never_establish_identity() {
        let a = Signature::new(LocalId(0), ExprId(1), vec![(LocalId(2), CellId(3))]);
        let mut b = Signature::new(LocalId(0), ExprId(1), vec![(LocalId(2), CellId(4))]);
        b.hash = a.hash;
        assert_ne!(a, b);
        let mut map = std::collections::HashMap::new();
        map.insert(a, "first");
        map.insert(b, "second");
        assert_eq!(map.len(), 2);
    }
}
