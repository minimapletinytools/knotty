//! Dynamic source validation for the existing staged graph importer. The code
//! map proves bodies, while this bounded ordered walk proves their actual reads.
use super::*;
use std::cell::{Cell, RefCell};

pub(super) struct Validation {
    old: u64,
    new: u64,
    limits: Limits,
    work: Cell<u64>,
    import_work: Cell<u64>,
    verdicts: RefCell<validation::Verdicts>,
    static_expressions: std::collections::HashSet<ExprId>,
    imported: RefCell<HashMap<storage::Id, evidence::Proof>>,
}
impl Validation {
    pub fn new(
        old: &Session,
        new: &Session,
        allowance: usize,
        static_expressions: std::collections::HashSet<ExprId>,
    ) -> Self {
        let mut limits = new.limits.clone();
        limits.steps = (allowance as u64).min(limits.steps.saturating_sub(new.stats.steps));
        Self {
            old: old.id,
            new: new.id,
            limits,
            work: Cell::new(0),
            import_work: Cell::new(0),
            verdicts: RefCell::new(Default::default()),
            static_expressions,
            imported: RefCell::new(HashMap::new()),
        }
    }
    pub fn work(&self) -> u64 {
        self.work.get() - self.import_work.get()
    }
    pub fn import_work(&self) -> u64 {
        self.import_work.get()
    }
    pub fn allowance(&self) -> u64 {
        self.limits.steps
    }
    pub fn import(
        &self,
        old: &Session,
        new: &mut Session,
        code: &knot_core::equivalence::Correspondence,
        proof: evidence::Proof,
    ) -> evidence::Proof {
        if !self.valid(old, new, code, proof) {
            return Default::default();
        }
        let mut work = self.work.get();
        let before = work;
        let imported = new.evidence.import_proof(
            &old.evidence,
            proof,
            &mut self.imported.borrow_mut(),
            (&self.limits, &mut work),
            |recipe| match recipe {
                evidence::Recipe::Thunk(_, Pending::Expr(expression, _)) => Some(
                    evidence::SummaryCode::Expression(*code.expressions.get(expression)?),
                ),
                evidence::Recipe::Call(function, _) => match &old.values[*function] {
                    Value::Closure(_, body, _) => Some(evidence::SummaryCode::Expression(
                        *code.expressions.get(body)?,
                    )),
                    _ => None,
                },
                evidence::Recipe::Historical(Some(expression)) => Some(
                    evidence::SummaryCode::Expression(*code.expressions.get(expression)?),
                ),
                evidence::Recipe::Summary(summary) => Some(match *summary {
                    evidence::SummaryCode::Expression(expr) => {
                        evidence::SummaryCode::Expression(*code.expressions.get(&expr)?)
                    }
                    evidence::SummaryCode::Call(param, body) => evidence::SummaryCode::Call(
                        *code.locals.get(&param)?,
                        *code.expressions.get(&body)?,
                    ),
                    evidence::SummaryCode::Region(root) => {
                        evidence::SummaryCode::Region(*code.expressions.get(&root)?)
                    }
                    evidence::SummaryCode::Immutable => evidence::SummaryCode::Immutable,
                }),
                evidence::Recipe::Thunk(..)
                | evidence::Recipe::Introduced(_)
                | evidence::Recipe::Host(..)
                | evidence::Recipe::Historical(None) => Some(evidence::SummaryCode::Immutable),
            },
        );
        self.import_work.set(self.import_work.get() + work - before);
        self.work.set(work);
        imported
    }
    pub fn valid(
        &self,
        old: &Session,
        new: &Session,
        code: &knot_core::equivalence::Correspondence,
        proof: evidence::Proof,
    ) -> bool {
        if self.old != old.id || self.new != new.id {
            return false;
        }
        let mut work = self.work.get();
        let valid = old
            .evidence
            .validate_with_code(
                proof,
                &new.hosts,
                &mut self.verdicts.borrow_mut(),
                &self.limits,
                &mut work,
                |recipe| {
                    use evidence::CodeValidity::{Changed, CheckReads, Proven};
                    let matches = match recipe {
                        evidence::Recipe::Thunk(_, Pending::Expr(expression, env)) => {
                            // Static correspondence covers every global/host read;
                            // a closed fragment has no potentially changed captures.
                            if !old.validation.changed()
                                && self.static_expressions.contains(expression)
                                && (old.envs[*env].empty
                                    || old
                                        .program
                                        .expressions
                                        .capture_layouts()
                                        .get(*expression)
                                        .is_some_and(<[LocalId]>::is_empty))
                            {
                                return Proven;
                            }
                            code.expressions.contains_key(expression)
                        }
                        evidence::Recipe::Call(function, _) => match &old.values[*function] {
                            Value::Closure(parameter, body, _) => {
                                code.locals.contains_key(parameter)
                                    && code.expressions.contains_key(body)
                            }
                            _ => false,
                        },
                        // Runtime edges/immutable payload have no inline editable
                        // code. Their demanded reads still validate normally.
                        evidence::Recipe::Thunk(..)
                        | evidence::Recipe::Introduced(_)
                        | evidence::Recipe::Host(..) => true,
                        evidence::Recipe::Historical(expression) => expression
                            .is_none_or(|expression| code.expressions.contains_key(&expression)),
                        evidence::Recipe::Summary(summary) => match summary {
                            evidence::SummaryCode::Expression(expr) => {
                                code.expressions.contains_key(expr)
                            }
                            evidence::SummaryCode::Call(param, body) => {
                                code.locals.contains_key(param)
                                    && code.expressions.contains_key(body)
                            }
                            evidence::SummaryCode::Region(root) => {
                                code.expressions.contains_key(root)
                            }
                            evidence::SummaryCode::Immutable => true,
                        },
                    };
                    if matches {
                        CheckReads
                    } else {
                        Changed
                    }
                },
            )
            .unwrap_or(false);
        self.work.set(work);
        valid
    }
}
