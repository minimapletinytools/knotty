//! Bounded, shared capture layouts for one immutable executable revision.
//! This proves lexical inputs only; cross-revision executable/host/dictionary
//! equivalence remains the responsibility of `equivalence`.
use crate::{ExprId, Kind, LocalId, Pattern};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

/// Optional execution layouts in one immutable arena.
/// Missing entries require ordinary lexical lookup; they never mean no inputs.
#[derive(Debug, Default)]
pub struct Captures {
    inputs: Inputs,
    prefix: usize,
    spent: usize,
}
// Copy only touched pages when checked code is appended. Metadata does not own
// a Program, runtime values, or an unbounded chain of prior metadata versions.
const PAGE: usize = 256;
type InputPage = [Option<Arc<[LocalId]>>; PAGE];
#[derive(Debug, Default, Clone)]
struct Inputs(HashMap<usize, Arc<InputPage>>);
impl Inputs {
    fn get(&self, id: &ExprId) -> Option<&Arc<[LocalId]>> {
        self.0.get(&(id.0 / PAGE))?[id.0 % PAGE].as_ref()
    }
    fn insert(&mut self, id: ExprId, inputs: Arc<[LocalId]>) {
        let page = self
            .0
            .entry(id.0 / PAGE)
            .or_insert_with(|| Arc::new(std::array::from_fn(|_| None)));
        Arc::make_mut(page)[id.0 % PAGE] = Some(inputs);
    }
}
impl Captures {
    pub(crate) fn prepare(expressions: &crate::Expressions, previous: Option<&Self>) -> Self {
        let mut layouts = Layouts {
            expressions,
            ready: previous.map(|p| p.inputs.clone()).unwrap_or_default(),
        };
        // Bound propagated sets as well as graph walks. This is preparation,
        // not evaluation: exhaustion only disables an optimization.
        let allowance = expressions.len().saturating_mul(64).min(8_000_000);
        let mut remaining = allowance.saturating_sub(previous.map_or(0, |p| p.spent));
        let prefix = previous.map_or(0, |p| p.prefix);
        for (index, expr) in expressions.iter_from(prefix).enumerate() {
            if remaining == 0 {
                break;
            }
            if matches!(expr.kind, Kind::Lam(..)) {
                layouts.get(ExprId(prefix + index), &mut remaining);
            }
        }
        Self {
            inputs: layouts.ready,
            prefix: expressions.len(),
            spent: allowance - remaining,
        }
    }
    pub fn get(&self, expression: ExprId) -> Option<&[LocalId]> {
        self.inputs.get(&expression).map(AsRef::as_ref)
    }
    pub(crate) fn precedes(&self, index: usize) -> bool {
        self.prefix <= index
    }
}

pub struct Layouts<'a> {
    expressions: &'a crate::Expressions,
    ready: Inputs,
}
impl<'a> Layouts<'a> {
    pub fn new(expressions: &'a crate::Expressions) -> Self {
        Self {
            expressions,
            ready: Inputs::default(),
        }
    }
    /// Every visited node, edge, binder and propagated input consumes work.
    /// A failed proof is a miss; a shared child is prepared only once.
    pub fn get(&mut self, root: ExprId, remaining: &mut usize) -> Option<Arc<[LocalId]>> {
        let mut todo = vec![(root, false)];
        let mut active = HashSet::new();
        while let Some((id, finish)) = todo.pop() {
            charge(remaining, 1)?;
            if self.ready.get(&id).is_some() {
                continue;
            }
            let kind = &self.expressions.get(id.0)?.kind;
            if finish {
                let inputs = self.combine(kind, remaining)?;
                self.ready.insert(id, inputs.into_iter().collect());
                active.remove(&id);
            } else {
                if !active.insert(id) {
                    return None;
                }
                todo.push((id, true));
                let children: Box<dyn Iterator<Item = ExprId> + '_> = match kind {
                    Kind::Lam(_, b) | Kind::Select(b, _) => Box::new(std::iter::once(*b)),
                    Kind::App(f, xs) => Box::new(std::iter::once(*f).chain(xs.iter().copied())),
                    Kind::Let(xs, b) => {
                        Box::new(xs.iter().map(|(_, e)| *e).chain(std::iter::once(*b)))
                    }
                    Kind::Case(s, xs) => {
                        Box::new(std::iter::once(*s).chain(xs.iter().map(|(_, e)| *e)))
                    }
                    Kind::Tuple(xs) => Box::new(xs.iter().copied()),
                    Kind::Record(xs) => Box::new(xs.iter().map(|(_, e)| *e)),
                    Kind::RecordUpdate(b, xs) => {
                        Box::new(std::iter::once(*b).chain(xs.iter().map(|(_, e)| *e)))
                    }
                    Kind::Dict(xs) => Box::new(xs.iter().map(|(_, e)| *e)),
                    Kind::Unsupported(_) => return None,
                    Kind::Rational(_)
                    | Kind::Lit(_)
                    | Kind::Local(_)
                    | Kind::Global(_)
                    | Kind::Prim(_)
                    | Kind::Host(..)
                    | Kind::Ctor(..) => Box::new(std::iter::empty()),
                };
                for child in children {
                    charge(remaining, 1)?;
                    todo.push((child, false));
                }
            }
        }
        self.ready.get(&root).cloned()
    }
    fn combine(&self, kind: &Kind, remaining: &mut usize) -> Option<BTreeSet<LocalId>> {
        if let Kind::Let(xs, _) = kind {
            charge(remaining, xs.len())?;
        }
        let mut result = BTreeSet::new();
        let mut include = |id: ExprId, bound: &BTreeSet<LocalId>| -> Option<()> {
            let inputs = self.ready.get(&id)?;
            charge(remaining, inputs.len())?;
            result.extend(inputs.iter().filter(|l| !bound.contains(l)).copied());
            Some(())
        };
        let empty = BTreeSet::new();
        match kind {
            Kind::Local(l) => {
                charge(remaining, 1)?;
                result.insert(*l);
            }
            Kind::Lam(l, b) => include(*b, &BTreeSet::from([*l]))?,
            Kind::Let(xs, b) => {
                let bound = xs.iter().map(|(l, _)| *l).collect();
                for (_, e) in xs {
                    include(*e, &bound)?;
                }
                include(*b, &bound)?;
            }
            Kind::Case(s, xs) => {
                // Separate branch binder sets: a binder in one arm cannot hide
                // an outer reference in the scrutinee or another arm.
                let mut branch_inputs = Vec::new();
                for (p, e) in xs {
                    let bound = binders(p, remaining)?;
                    branch_inputs.push((*e, bound));
                }
                let mut include = |id: ExprId, bound: &BTreeSet<LocalId>| -> Option<()> {
                    let inputs = self.ready.get(&id)?;
                    charge(remaining, inputs.len())?;
                    result.extend(inputs.iter().filter(|l| !bound.contains(l)).copied());
                    Some(())
                };
                include(*s, &empty)?;
                for (e, bound) in branch_inputs {
                    include(e, &bound)?;
                }
            }
            Kind::Select(e, _) => include(*e, &empty)?,
            Kind::App(e, xs) => {
                include(*e, &empty)?;
                for x in xs {
                    include(*x, &empty)?;
                }
            }
            Kind::Tuple(xs) => {
                for x in xs {
                    include(*x, &empty)?;
                }
            }
            Kind::Record(xs) => {
                for (_, x) in xs {
                    include(*x, &empty)?;
                }
            }
            Kind::Dict(xs) => {
                for (_, x) in xs {
                    include(*x, &empty)?;
                }
            }
            Kind::RecordUpdate(e, xs) => {
                include(*e, &empty)?;
                for (_, x) in xs {
                    include(*x, &empty)?;
                }
            }
            Kind::Unsupported(_) => return None,
            Kind::Rational(_)
            | Kind::Lit(_)
            | Kind::Global(_)
            | Kind::Prim(_)
            | Kind::Host(..)
            | Kind::Ctor(..) => {}
        }
        Some(result)
    }
}
fn charge(remaining: &mut usize, count: usize) -> Option<()> {
    match remaining.checked_sub(count) {
        Some(left) => {
            *remaining = left;
            Some(())
        }
        None => {
            *remaining = 0;
            None
        }
    }
}
fn binders(pattern: &Pattern, remaining: &mut usize) -> Option<BTreeSet<LocalId>> {
    let mut bound = BTreeSet::new();
    let mut todo = vec![pattern];
    while let Some(p) = todo.pop() {
        charge(remaining, 1)?;
        match p {
            Pattern::Bind(l) => {
                bound.insert(*l);
            }
            Pattern::As(l, p) => {
                bound.insert(*l);
                todo.push(p);
            }
            Pattern::Tuple(ps) | Pattern::Ctor(_, ps) => {
                charge(remaining, ps.len())?;
                todo.extend(ps);
            }
            Pattern::Record(xs) => {
                charge(remaining, xs.len())?;
                bound.extend(xs.iter().map(|(_, l)| *l));
            }
            Pattern::Wildcard | Pattern::Lit(_) => {}
        }
    }
    Some(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Program;
    fn push(p: &mut Program, kind: Kind) -> ExprId {
        static ORIGIN: std::sync::OnceLock<crate::Origin> = std::sync::OnceLock::new();
        let origin = *ORIGIN.get_or_init(|| {
            let parsed = knot_syntax::parse("module Test exposing (..)\nx = ()\n").unwrap();
            let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
            let checked = knot_checker::check::check_program(&canonical).unwrap();
            crate::lower(&[checked]).expressions[0].origin
        });
        p.push(origin, kind)
    }
    #[test]
    fn binders_are_scoped_and_layouts_are_shared_without_runtime_inputs() {
        let mut p = Program::default();
        let x = push(&mut p, Kind::Local(LocalId(0)));
        let y = push(&mut p, Kind::Local(LocalId(1)));
        let pair = push(&mut p, Kind::Tuple(vec![x, y]));
        let lambda = push(&mut p, Kind::Lam(LocalId(0), pair));
        let branch = push(
            &mut p,
            Kind::Case(
                y,
                vec![(Pattern::Bind(LocalId(0)), x), (Pattern::Wildcard, x)],
            ),
        );
        let rec = push(&mut p, Kind::Let(vec![(LocalId(0), pair)], x));
        let mut layouts = Layouts::new(&p.expressions);
        let a = layouts.get(lambda, &mut 100).unwrap();
        assert_eq!(&*a, &[LocalId(1)]);
        assert!(Arc::ptr_eq(&a, &layouts.get(lambda, &mut 1).unwrap()));
        assert_eq!(
            &*layouts.get(branch, &mut 100).unwrap(),
            &[LocalId(0), LocalId(1)]
        );
        assert_eq!(&*layouts.get(rec, &mut 100).unwrap(), &[LocalId(1)]);
    }
    #[test]
    fn exhaustion_unsupported_edges_and_cycles_are_misses_not_closed_layouts() {
        let mut p = Program::default();
        let x = push(&mut p, Kind::Local(LocalId(0)));
        let wide = push(&mut p, Kind::Tuple(vec![x; 1024]));
        let cycle = push(&mut p, Kind::Tuple(vec![]));
        p.expressions[cycle.0].kind = Kind::Tuple(vec![cycle]);
        let unsupported = push(&mut p, Kind::Unsupported("unknown".into()));
        let mut layouts = Layouts::new(&p.expressions);
        assert!(layouts.get(wide, &mut 10).is_none());
        assert!(layouts.get(cycle, &mut 100).is_none());
        assert!(layouts.get(unsupported, &mut 100).is_none());
        assert_eq!(&*layouts.get(wide, &mut 10000).unwrap(), &[LocalId(0)]);
    }

    #[test]
    fn prepared_layouts_follow_all_mutation_paths_and_leave_forks_unchanged() {
        let mut p = Program::default();
        let x = push(&mut p, Kind::Local(LocalId(0)));
        let lambda = push(&mut p, Kind::Lam(LocalId(1), x));
        assert_eq!(
            p.expressions.capture_layouts().get(lambda),
            Some(&[LocalId(0)][..])
        );
        let mut fork = p.clone();
        fork.expressions[x.0].kind = Kind::Local(LocalId(2));
        assert_eq!(
            fork.expressions.capture_layouts().get(lambda),
            Some(&[LocalId(2)][..])
        );
        assert_eq!(
            p.expressions.capture_layouts().get(lambda),
            Some(&[LocalId(0)][..])
        );
        for expr in &mut fork.expressions {
            if matches!(expr.kind, Kind::Local(_)) {
                expr.kind = Kind::Local(LocalId(3));
            }
        }
        assert_eq!(
            fork.expressions.capture_layouts().get(lambda),
            Some(&[LocalId(3)][..])
        );
        let closed = push(&mut fork, Kind::Lam(LocalId(3), x));
        assert_eq!(
            fork.expressions.capture_layouts().get(closed),
            Some(&[][..])
        );
        assert!(p.expressions.capture_layouts().get(closed).is_none());
    }

    #[test]
    fn deeply_nested_shared_code_is_iterative_and_missing_edges_are_unknown() {
        let mut p = Program::default();
        let mut e = push(&mut p, Kind::Local(LocalId(0)));
        for _ in 0..20_000 {
            e = push(&mut p, Kind::Tuple(vec![e, e]));
        }
        let bad = push(&mut p, Kind::Tuple(vec![ExprId(usize::MAX)]));
        let mut layouts = Layouts::new(&p.expressions);
        assert_eq!(&*layouts.get(e, &mut 1_000_000).unwrap(), &[LocalId(0)]);
        assert!(layouts.get(bad, &mut 100).is_none());
    }

    #[test]
    fn appended_code_reuses_proved_inputs_without_revisiting_the_prefix() {
        let mut p = Program::default();
        let x = push(&mut p, Kind::Local(LocalId(0)));
        let mut body = x;
        for _ in 0..2000 {
            body = push(&mut p, Kind::Tuple(vec![body, body]));
        }
        let lambda = push(&mut p, Kind::Lam(LocalId(1), body));
        let before = p.expressions.capture_layouts();
        let proof = before.inputs.get(&lambda).unwrap().clone();
        let spent = before.spent;
        let mut fork = p.clone();
        let added = push(&mut fork, Kind::Lam(LocalId(0), body));
        // Linkers may rewrite the new suffix before sealing the extension.
        fork.expressions[added.0].kind = Kind::Lam(LocalId(0), body);
        let after = fork.expressions.capture_layouts();
        assert!(after.spent > spent && after.spent - spent < 10);
        assert!(Arc::ptr_eq(&proof, after.inputs.get(&lambda).unwrap()));
        assert_eq!(after.get(added), Some(&[][..]));
        assert!(p.expressions.capture_layouts().get(added).is_none());
        fork.expressions[x.0].kind = Kind::Local(LocalId(2));
        assert_eq!(
            fork.expressions.capture_layouts().get(added),
            Some(&[LocalId(2)][..])
        );
        assert_eq!(
            p.expressions.capture_layouts().get(lambda),
            Some(&[LocalId(0)][..])
        );
    }
}
