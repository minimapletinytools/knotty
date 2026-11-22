//! Align changed bodies, then prove exact executable fragments bottom-up. Local
//! roles may align across a changed parent; only exact code receives an ExprId
//! correspondence. Neither relation proves unchanged runtime inputs or values.
use super::*;

struct Aligned {
    exact: bool,
    children: Vec<(ExprId, ExprId)>,
    locals: Vec<(LocalId, LocalId)>,
}
enum Visit {
    Enter(ExprId, ExprId),
    Exit(ExprId, ExprId, Aligned),
}

pub(super) fn extend(
    result: &mut BodyVersions<'_>,
    roots: &[Global],
    remaining: &mut usize,
) -> Option<()> {
    // Bound scratch maps before cloning whole-body correspondences.
    *remaining =
        remaining.checked_sub(result.expressions.len().checked_add(result.locals.len())?)?;
    let mut expressions_back = result.expressions.iter().map(|(a, b)| (*b, *a)).collect();
    let mut locals_back = result.locals.iter().map(|(a, b)| (*b, *a)).collect();
    let mut seen = BTreeSet::new();
    for root in roots {
        if *remaining == 0 {
            break;
        }
        *remaining -= 1;
        if result.contains(root) {
            continue;
        }
        let (Some(a), Some(b)) = (result.old.globals.get(root), result.new.globals.get(root))
        else {
            continue;
        };
        let mut pending = vec![Visit::Enter(*a, *b)];
        while let Some(visit) = pending.pop() {
            if *remaining == 0 {
                return Some(());
            }
            *remaining -= 1;
            match visit {
                Visit::Enter(a, b) => {
                    if let Some(previous) = result.expressions.get(&a) {
                        if *previous != b {
                            return None;
                        }
                        continue;
                    }
                    if !seen.insert((a, b)) {
                        continue;
                    }
                    let (Some(x), Some(y)) = (
                        result.old.expressions.get(a.0),
                        result.new.expressions.get(b.0),
                    ) else {
                        continue;
                    };
                    let Some(aligned) = align(&x.kind, &y.kind, &result.locals, remaining) else {
                        return Some(());
                    };
                    for (a, b) in &aligned.locals {
                        pair(&mut result.locals, &mut locals_back, *a, *b)?;
                    }
                    // Each queued edge and its exit check was charged by align.
                    let children = aligned.children.clone();
                    pending.push(Visit::Exit(a, b, aligned));
                    pending.extend(children.into_iter().map(|(a, b)| Visit::Enter(a, b)));
                }
                Visit::Exit(a, b, aligned) => {
                    if aligned.exact
                        && aligned
                            .children
                            .iter()
                            .all(|(a, b)| result.expression(*a) == Some(*b))
                    {
                        pair(&mut result.expressions, &mut expressions_back, a, b)?;
                    }
                }
            }
        }
    }
    Some(())
}

fn width(kind: &Kind) -> usize {
    match kind {
        Kind::App(_, xs) | Kind::Tuple(xs) => xs.len(),
        Kind::Let(xs, _) => xs.len(),
        Kind::Case(_, xs) => xs.len(),
        Kind::Record(xs) | Kind::RecordUpdate(_, xs) => xs.len(),
        Kind::Dict(xs) => xs.len(),
        _ => 0,
    }
}
fn align(
    a: &Kind,
    b: &Kind,
    locals: &HashMap<LocalId, LocalId>,
    remaining: &mut usize,
) -> Option<Aligned> {
    // Includes child staging, field alignment and the final child-map checks.
    let work = width(a)
        .checked_add(width(b))?
        .checked_add(1)?
        .checked_mul(4)?;
    *remaining = remaining.checked_sub(work)?;
    let mut out = Aligned {
        exact: false,
        children: vec![],
        locals: vec![],
    };
    out.exact = match (a, b) {
        (Kind::Local(a), Kind::Local(b)) => locals.get(a) == Some(b),
        (Kind::Lit(a), Kind::Lit(b)) => same_literal(a, b),
        (Kind::Lam(a, x), Kind::Lam(b, y)) => {
            out.locals.push((*a, *b));
            out.children.push((*x, *y));
            true
        }
        (Kind::Select(a, x), Kind::Select(b, y)) => {
            out.children.push((*a, *b));
            x == y
        }
        (Kind::App(a, xs), Kind::App(b, ys)) if xs.len() == ys.len() => {
            out.children.push((*a, *b));
            out.children
                .extend(xs.iter().copied().zip(ys.iter().copied()));
            true
        }
        (Kind::Let(xs, a), Kind::Let(ys, b)) if xs.len() == ys.len() => {
            for ((x, a), (y, b)) in xs.iter().zip(ys) {
                out.locals.push((*x, *y));
                out.children.push((*a, *b));
            }
            out.children.push((*a, *b));
            true
        }
        (Kind::Case(a, xs), Kind::Case(b, ys)) if xs.len() == ys.len() => {
            out.children.push((*a, *b));
            let mut binders = vec![];
            let patterns_match = xs.iter().zip(ys).all(|((a, _), (b, _))| {
                super::super::fragments::pattern_context(a, b, &mut binders, remaining)
            });
            if patterns_match {
                out.locals = binders;
                out.children
                    .extend(xs.iter().zip(ys).map(|((_, a), (_, b))| (*a, *b)));
            }
            patterns_match
        }
        (Kind::Tuple(xs), Kind::Tuple(ys)) if xs.len() == ys.len() => {
            out.children
                .extend(xs.iter().copied().zip(ys.iter().copied()));
            true
        }
        (Kind::Record(xs), Kind::Record(ys)) => fields(xs, ys, &mut out.children),
        (Kind::RecordUpdate(a, xs), Kind::RecordUpdate(b, ys)) => {
            out.children.push((*a, *b));
            fields(xs, ys, &mut out.children)
        }
        (Kind::Dict(xs), Kind::Dict(ys)) => fields(xs, ys, &mut out.children),
        // Only leaf kinds may use derived equality. Compound equality would
        // confuse coincidentally equal arena IDs with proved correspondence.
        (Kind::Rational(_), Kind::Rational(_))
        | (Kind::Global(_), Kind::Global(_))
        | (Kind::Prim(_), Kind::Prim(_))
        | (Kind::Host(..), Kind::Host(..))
        | (Kind::Ctor(..), Kind::Ctor(..)) => a == b,
        _ => false,
    };
    Some(out)
}
fn fields<K: Ord>(xs: &[(K, ExprId)], ys: &[(K, ExprId)], out: &mut Vec<(ExprId, ExprId)>) -> bool {
    let right = ys
        .iter()
        .map(|(k, v)| (k, *v))
        .collect::<std::collections::BTreeMap<_, _>>();
    out.extend(
        xs.iter()
            .filter_map(|(k, a)| right.get(k).map(|b| (*a, *b))),
    );
    xs.len() == ys.len() && xs.iter().zip(ys).all(|((a, _), (b, _))| a == b)
}
