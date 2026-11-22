//! Bounded lexical code guards. A region is an actual lambda expression, whose
//! complete syntax (including its parameter) must still match in the receiving
//! program. Global references are leaves: their demanded values remain reads.
//! This metadata owns neither a Program nor runtime handles.
use super::*;

const ABSENT: ExprId = ExprId(usize::MAX);
const MAX_EXPRESSIONS: usize = 200_000;
const CALL_BYTES: usize = 128;

pub(super) struct Regions {
    owners: Vec<ExprId>,
    calls: HashMap<(LocalId, ExprId), Option<ExprId>>,
    bytes: usize,
}

impl Regions {
    fn prepare(program: &Program, limits: &Limits, work: &mut u64) -> Option<Self> {
        let count = program.expressions.len();
        if count > MAX_EXPRESSIONS {
            return None;
        }
        let scratch = count.checked_mul(std::mem::size_of::<ExprId>())?;
        // Arc/header storage and the hash table's minimum bucket/control
        // allocation matter even for a single lambda (or an empty map).
        let base = scratch.checked_add(std::mem::size_of::<Self>() + 64)?;
        if base.checked_add(scratch)? > limits.dependency_bytes {
            return None;
        }
        let mut lambdas = 0usize;
        for expr in &program.expressions {
            if limits.cancellation.load(Ordering::Relaxed) || !charge(work, limits) {
                return None;
            }
            lambdas += usize::from(matches!(expr.kind, Kind::Lam(..)));
        }
        let bytes = base.checked_add(lambdas.checked_mul(CALL_BYTES)?)?;
        // Bound retained metadata and the worst-case unique-node worklist before
        // allocation. Optional preparation may decline without disabling evidence.
        if bytes.checked_add(scratch)? > limits.dependency_bytes
            || count as u64 > limits.steps.saturating_sub(*work)
        {
            return None;
        }
        *work += count as u64;
        let mut result = Self {
            owners: vec![ABSENT; count],
            calls: HashMap::with_capacity(lambdas),
            bytes,
        };
        let mut pending = Vec::with_capacity(count);
        for (index, expression) in program.expressions.iter().enumerate() {
            if limits.cancellation.load(Ordering::Relaxed) || !charge(work, limits) {
                return None;
            }
            let Kind::Lam(parameter, body) = expression.kind else {
                continue;
            };
            let root = ExprId(index);
            result
                .calls
                .entry((parameter, body))
                .and_modify(|old| {
                    if *old != Some(root) {
                        *old = None;
                    }
                })
                .or_insert(Some(root));
            result.owners[index] = root;
            pending.push(root);
            while let Some(id) = pending.pop() {
                if limits.cancellation.load(Ordering::Relaxed) || !charge(work, limits) {
                    return None;
                }
                let expr = program.expressions.get(id.0)?;
                let mut child = |id: ExprId| -> Option<()> {
                    if !charge(work, limits) {
                        return None;
                    }
                    let node = program.expressions.get(id.0)?;
                    // Each nested lambda supplies its own complete version guard.
                    // A shared non-lambda node may choose either containing guard;
                    // that can add invalidations, never omit required code checks.
                    if !matches!(node.kind, Kind::Lam(..)) && result.owners[id.0] == ABSENT {
                        result.owners[id.0] = root;
                        pending.push(id);
                    }
                    Some(())
                };
                match &expr.kind {
                    Kind::Lam(_, body) | Kind::Select(body, _) => child(*body)?,
                    Kind::App(f, args) => {
                        child(*f)?;
                        for arg in args {
                            child(*arg)?;
                        }
                    }
                    Kind::Let(bindings, body) => {
                        for (_, expr) in bindings {
                            child(*expr)?;
                        }
                        child(*body)?;
                    }
                    Kind::Case(value, arms) => {
                        child(*value)?;
                        for (_, expr) in arms {
                            child(*expr)?;
                        }
                    }
                    Kind::Tuple(fields) => {
                        for expr in fields {
                            child(*expr)?;
                        }
                    }
                    Kind::Record(fields) => {
                        for (_, expr) in fields {
                            child(*expr)?;
                        }
                    }
                    Kind::RecordUpdate(value, fields) => {
                        child(*value)?;
                        for (_, expr) in fields {
                            child(*expr)?;
                        }
                    }
                    Kind::Dict(fields) => {
                        for (_, expr) in fields {
                            child(*expr)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(result)
    }

    fn guard(&self, code: SummaryCode) -> SummaryCode {
        let root = match code {
            SummaryCode::Expression(expr) => self
                .owners
                .get(expr.0)
                .copied()
                .filter(|root| *root != ABSENT),
            SummaryCode::Call(param, body) => self.calls.get(&(param, body)).copied().flatten(),
            SummaryCode::Region(_) | SummaryCode::Immutable => None,
        };
        root.map(SummaryCode::Region).unwrap_or(code)
    }
}

impl Store {
    /// Inline only guards already covered by this result's lexical code guard.
    /// Boundary reads retain first-observation order. Optional reduction is
    /// all-or-nothing, with fixed scratch space and a small metered work ceiling.
    pub(super) fn normalize_region_reads(
        &mut self,
        code: SummaryCode,
        reads: &mut Vec<Proof>,
        limits: &Limits,
        work: &mut u64,
    ) {
        const WIDTH: usize = 32;
        const WORK: u64 = 128;
        if !limits.region_evidence
            || !matches!(code, SummaryCode::Region(_))
            || reads.len() > WIDTH
            || limits.steps.saturating_sub(*work) < WORK
            || limits.cancellation.load(Ordering::Relaxed)
        {
            return;
        }
        let mut output = [Proof::default(); WIDTH];
        let mut len = 0;
        let mut spent = 0;
        let complete = (|| {
            for read in reads.iter() {
                spent += 1;
                if spent > WORK {
                    return None;
                }
                let record = self.get(*read)?;
                let children = match record.recipe {
                    Recipe::Summary(child) if child == code || child == SummaryCode::Immutable => {
                        record.reads.as_ref()
                    }
                    _ => std::slice::from_ref(read),
                };
                for child in children {
                    spent += 1;
                    if spent > WORK {
                        return None;
                    }
                    let mut duplicate = false;
                    for previous in &output[..len] {
                        spent += 1;
                        if spent > WORK {
                            return None;
                        }
                        if previous == child {
                            duplicate = true;
                            break;
                        }
                    }
                    if !duplicate {
                        if len == WIDTH {
                            return None;
                        }
                        output[len] = *child;
                        len += 1;
                    }
                }
            }
            Some(())
        })()
        .is_some();
        *work += spent.min(WORK);
        let bytes = BASE_BYTES.saturating_add(len.saturating_mul(READ_BYTES));
        if complete
            && !limits.cancellation.load(Ordering::Relaxed)
            && output[..len] != reads[..]
            && (len <= reads.len() || self.room(len, bytes, limits))
        {
            reads.clear();
            reads.extend_from_slice(&output[..len]);
            self.stats.region_normalizations = self.stats.region_normalizations.saturating_add(1);
        }
    }

    pub(in crate::machine) fn prepare_regions(
        &mut self,
        program: &Program,
        limits: &Limits,
        work: &mut u64,
    ) {
        if limits.region_evidence && limits.compact_evidence && !self.regions_attempted {
            self.regions_attempted = true;
            // Preparation happens on first execution, after fresh-session reuse.
            // Imported proofs already consume part of the storage allowance.
            let mut available = limits.clone();
            available.dependency_bytes = limits
                .dependency_bytes
                .saturating_sub(self.stats.bytes)
                .saturating_sub(self.active_bytes);
            // Optional metadata must not exhaust a small execution allowance.
            available.steps = (*work).saturating_add(limits.steps.saturating_sub(*work) / 8);
            if let Some(regions) = Regions::prepare(program, &available, work) {
                self.stats.bytes += regions.bytes;
                self.stats.region_bytes = regions.bytes;
                self.regions = Some(Arc::new(regions));
            }
        }
    }

    pub(super) fn region_guard(&self, code: SummaryCode, limits: &Limits) -> SummaryCode {
        if limits.region_evidence {
            self.regions
                .as_ref()
                .map_or(code, |regions| regions.guard(code))
        } else {
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            compact_evidence: true,
            region_evidence: true,
            ..Default::default()
        }
    }
    fn program(kinds: Vec<Kind>) -> Program {
        let parsed = knot_syntax::parse("module Test exposing (..)\nx = ()\n").unwrap();
        let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
        let checked = knot_checker::check::check_program(&canonical).unwrap();
        let source = knot_core::lower(&[checked]);
        let origin = source.expressions[0].origin;
        let mut result = Program::default();
        for kind in kinds {
            result.push(origin, kind);
        }
        result
    }
    #[test]
    fn lexical_guards_do_not_absorb_caller_arguments_or_nested_parameters() {
        let p = program(vec![
            Kind::Local(LocalId(0)), // callee body
            Kind::Lam(LocalId(0), ExprId(0)),
            Kind::Lit(Literal::Int(42)), // caller argument, outside lambda
            Kind::App(ExprId(1), vec![ExprId(2)]),
            Kind::Lam(LocalId(1), ExprId(1)), // curried outer parameter
            Kind::Lam(LocalId(2), ExprId(0)), // shared syntax, different parameter
        ]);
        let r = Regions::prepare(&p, &limits(), &mut 0).unwrap();
        assert_eq!(
            r.guard(SummaryCode::Expression(ExprId(0))),
            SummaryCode::Region(ExprId(1))
        );
        assert_eq!(
            r.guard(SummaryCode::Expression(ExprId(2))),
            SummaryCode::Expression(ExprId(2))
        );
        assert_eq!(
            r.guard(SummaryCode::Call(LocalId(1), ExprId(1))),
            SummaryCode::Region(ExprId(4))
        );
        assert_eq!(
            r.guard(SummaryCode::Call(LocalId(2), ExprId(0))),
            SummaryCode::Region(ExprId(5))
        );
        assert_eq!(
            r.guard(SummaryCode::Expression(ExprId(999))),
            SummaryCode::Expression(ExprId(999))
        );
        let mut duplicate = p;
        duplicate.push(
            duplicate.expressions[0].origin,
            Kind::Lam(LocalId(0), ExprId(0)),
        );
        let r = Regions::prepare(&duplicate, &limits(), &mut 0).unwrap();
        assert_eq!(
            r.guard(SummaryCode::Call(LocalId(0), ExprId(0))),
            SummaryCode::Call(LocalId(0), ExprId(0))
        );
    }
    #[test]
    fn bounded_preparation_and_rollback_never_publish_partial_metadata() {
        let p = program(vec![
            Kind::Lit(Literal::Unit),
            Kind::Lam(LocalId(0), ExprId(0)),
        ]);
        for limits in [
            Limits {
                dependency_bytes: 0,
                ..limits()
            },
            Limits {
                steps: 3,
                ..limits()
            },
        ] {
            let mut s = Store::default();
            let mut work = 0;
            s.prepare_regions(&p, &limits, &mut work);
            assert!(s.regions.is_none());
            assert_eq!(s.stats.bytes, 0);
            assert!(work <= limits.steps);
        }
        let mut s = Store::default();
        let snap = s.snapshot();
        s.prepare_regions(&p, &limits(), &mut 0);
        assert!(s.regions.is_some());
        assert!(s.stats.region_bytes > 0);
        s.rollback(snap);
        assert!(s.regions.is_none());
        assert!(!s.regions_attempted);
        assert_eq!(s.stats.bytes, 0);
        let cancelled = limits();
        cancelled.cancellation.store(true, Ordering::Relaxed);
        assert!(Regions::prepare(&p, &cancelled, &mut 0).is_none());
        let invalid = program(vec![Kind::Lam(LocalId(0), ExprId(100))]);
        assert!(Regions::prepare(&invalid, &limits(), &mut 0).is_none());
        // A malformed cyclic syntax graph cannot make metadata preparation loop.
        let cyclic = program(vec![
            Kind::Select(ExprId(0), Field::Index(0)),
            Kind::Lam(LocalId(0), ExprId(0)),
        ]);
        assert!(Regions::prepare(&cyclic, &limits(), &mut 0).is_some());
    }
    #[test]
    fn reduction_preserves_external_boundaries_and_first_read_order() {
        let limits = limits();
        let mut s = Store::default();
        let mut work = 0;
        let input = s.insert_summary(
            SummaryCode::Expression(ExprId(0)),
            vec![],
            &limits,
            &mut work,
        );
        let callback = s.insert_summary(SummaryCode::Region(ExprId(1)), vec![], &limits, &mut work);
        let immutable = s.insert_summary(SummaryCode::Immutable, vec![], &limits, &mut work);
        let code = SummaryCode::Region(ExprId(2));
        let child = s.insert_summary(code, vec![input, callback], &limits, &mut work);
        let parent = s.insert_summary(
            code,
            vec![immutable, child, input, callback],
            &limits,
            &mut work,
        );
        assert_eq!(child, parent);
        assert_eq!(s.get(parent).unwrap().reads.as_ref(), &[input, callback]);
        assert!(s.stats.region_normalizations > 0);
        // Inputs from distinct invocations are not combined into a region-wide union.
        let other = s.insert_summary(
            SummaryCode::Expression(ExprId(3)),
            vec![],
            &limits,
            &mut work,
        );
        let second = s.insert_summary(code, vec![other], &limits, &mut work);
        assert_ne!(second, parent);
        assert_eq!(s.get(second).unwrap().reads.as_ref(), &[other]);
    }
    #[test]
    fn reduction_at_capacity_and_transfer_still_require_complete_region_guards() {
        let limits = limits();
        let mut source = Store::default();
        let mut work = 0;
        let input = source.insert_summary(
            SummaryCode::Expression(ExprId(0)),
            vec![],
            &limits,
            &mut work,
        );
        let code = SummaryCode::Region(ExprId(1));
        let child = source.insert_summary(code, vec![input], &limits, &mut work);
        let full = Limits {
            dependency_records: source.stats.certificates,
            ..limits.clone()
        };
        assert_eq!(
            child,
            source.insert_summary(code, vec![child, input], &full, &mut work)
        );
        let mut target = Store::default();
        let missing = target.import_proof(
            &source,
            child,
            &mut HashMap::new(),
            (&limits, &mut work),
            |recipe| match recipe {
                Recipe::Summary(SummaryCode::Expression(_)) => {
                    Some(SummaryCode::Expression(ExprId(100)))
                }
                _ => None,
            },
        );
        assert!(
            missing.id.is_none(),
            "a mapped input cannot substitute for the complete lambda guard"
        );
        let mapped = target.import_proof(
            &source,
            child,
            &mut HashMap::new(),
            (&limits, &mut work),
            |recipe| match recipe {
                Recipe::Summary(SummaryCode::Expression(_)) => {
                    Some(SummaryCode::Expression(ExprId(100)))
                }
                Recipe::Summary(SummaryCode::Region(_)) => Some(SummaryCode::Region(ExprId(101))),
                _ => None,
            },
        );
        assert!(matches!(
            target.get(mapped).unwrap().recipe,
            Recipe::Summary(SummaryCode::Region(ExprId(101)))
        ));
        // Even an immutable receipt must retain reads if it has any.
        let wrapper =
            source.insert_summary(SummaryCode::Immutable, vec![input], &limits, &mut work);
        assert_eq!(
            child,
            source.insert_summary(code, vec![wrapper], &limits, &mut work)
        );
    }

    #[test]
    fn wide_missing_or_over_budget_reductions_keep_the_entire_original_proof() {
        let limits = limits();
        let mut s = Store::default();
        let mut work = 0;
        let code = SummaryCode::Region(ExprId(0));
        let boundary = s.insert_summary(
            SummaryCode::Expression(ExprId(1)),
            vec![],
            &limits,
            &mut work,
        );
        let child = s.insert_summary(code, vec![boundary], &limits, &mut work);
        for original in [vec![child; 33], vec![child, Proof::default()]] {
            let mut reads = original.clone();
            s.normalize_region_reads(code, &mut reads, &limits, &mut work);
            assert_eq!(reads, original);
        }
        let small = Limits {
            steps: work + 127,
            ..limits.clone()
        };
        let mut reads = vec![child, child];
        s.normalize_region_reads(code, &mut reads, &small, &mut work);
        assert_eq!(reads, vec![child, child]);
        // Distinct boundary reads exhaust comparison work before width.
        let mut many = Vec::new();
        for n in 10..40 {
            many.push(s.insert_summary(
                SummaryCode::Expression(ExprId(n)),
                vec![],
                &limits,
                &mut work,
            ));
        }
        let original = many.clone();
        let before = work;
        s.normalize_region_reads(code, &mut many, &limits, &mut work);
        assert_eq!(many, original);
        assert!(work - before <= 128);
    }
}
