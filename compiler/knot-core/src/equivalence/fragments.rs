use super::*;

pub(super) struct Observed<'a> {
    pub expressions: &'a [ExprId],
    pub captures: bool,
}

pub(super) fn extend(
    old: &Program,
    new: &Program,
    roots: &[Global],
    observed: Observed<'_>,
    mapping: &mut Correspondence,
    remaining: &mut usize,
    hosts: &BTreeSet<GlobalName>,
) {
    if observed.expressions.is_empty() {
        return;
    }
    let wanted = observed
        .expressions
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for root in roots {
        if mapping.globals.contains(root) {
            continue;
        }
        let (Some(a), Some(b)) = (old.globals.get(root), new.globals.get(root)) else {
            continue;
        };
        let mut pending = vec![(*a, *b)];
        while let Some((a, b)) = pending.pop() {
            if *remaining == 0 {
                return;
            }
            *remaining -= 1;
            if !seen.insert((a, b)) {
                continue;
            }
            if observed.captures && !context(old, new, a, b, mapping, remaining) {
                continue;
            }
            if wanted.contains(&a) {
                let shapes = shape_with_globals(old, a, remaining, Some(&mapping.globals))
                    .and_then(|left| {
                        // Keep this lazy: a rejected old dependency must not
                        // consume comparison budget for the new subtree.
                        let right = shape_with_globals(new, b, remaining, Some(&mapping.globals))?;
                        Some((left, right))
                    });
                if let Some((left, right)) = shapes {
                    if (observed.captures || (left.closed && right.closed))
                        && same_kinds(&left.kinds, &right.kinds)
                        && left.globals.is_subset(&mapping.globals)
                        && !left.kinds.iter().any(|k| {
                            matches!(k, Kind::Unsupported(_))
                                || matches!(k, Kind::Host(n, _) if !hosts.contains(n))
                        })
                        && left
                            .expressions
                            .iter()
                            .zip(&right.expressions)
                            .all(|(a, b)| {
                                mapping
                                    .expressions
                                    .get(a)
                                    .is_none_or(|previous| previous == b)
                            })
                        && left.locals.iter().zip(&right.locals).all(|(a, b)| {
                            mapping.locals.get(a).is_none_or(|previous| previous == b)
                        })
                    {
                        mapping.fragments.extend(left.expressions.iter().copied());
                        mapping
                            .expressions
                            .extend(left.expressions.into_iter().zip(right.expressions));
                        mapping
                            .locals
                            .extend(left.locals.into_iter().zip(right.locals));
                        continue;
                    }
                }
            }
            let (Some(a), Some(b)) = (old.expressions.get(a.0), new.expressions.get(b.0)) else {
                continue;
            };
            match (&a.kind, &b.kind) {
                (Kind::Lam(_, a), Kind::Lam(_, b)) | (Kind::Select(a, _), Kind::Select(b, _)) => {
                    pending.push((*a, *b))
                }
                (Kind::App(a, xs), Kind::App(b, ys))
                    if xs.len() == ys.len() && xs.len() < *remaining =>
                {
                    pending.push((*a, *b));
                    pending.extend(xs.iter().copied().zip(ys.iter().copied()));
                }
                (Kind::Let(xs, a), Kind::Let(ys, b))
                    if xs.len() == ys.len() && xs.len() < *remaining =>
                {
                    pending.push((*a, *b));
                    pending.extend(xs.iter().zip(ys).map(|((_, a), (_, b))| (*a, *b)));
                }
                (Kind::Case(a, xs), Kind::Case(b, ys))
                    if xs.len() == ys.len() && xs.len() < *remaining =>
                {
                    pending.push((*a, *b));
                    pending.extend(xs.iter().zip(ys).map(|((_, a), (_, b))| (*a, *b)));
                }
                (Kind::Tuple(xs), Kind::Tuple(ys))
                    if xs.len() == ys.len() && xs.len() < *remaining =>
                {
                    pending.extend(xs.iter().copied().zip(ys.iter().copied()))
                }
                (Kind::Record(xs), Kind::Record(ys)) => fields(xs, ys, &mut pending, remaining),
                (Kind::RecordUpdate(a, xs), Kind::RecordUpdate(b, ys)) => {
                    pending.push((*a, *b));
                    fields(xs, ys, &mut pending, remaining);
                }
                (Kind::Dict(xs), Kind::Dict(ys)) => fields(xs, ys, &mut pending, remaining),
                _ => {}
            }
            // Alignment work is bounded even for extremely wide products.
            if pending.len() > *remaining {
                return;
            }
        }
    }
}
fn fields<K: Ord>(
    xs: &[(K, ExprId)],
    ys: &[(K, ExprId)],
    pending: &mut Vec<(ExprId, ExprId)>,
    remaining: &mut usize,
) {
    let Some(left) = remaining.checked_sub(xs.len().saturating_add(ys.len())) else {
        *remaining = 0;
        return;
    };
    *remaining = left;
    let right = ys
        .iter()
        .map(|(k, v)| (k, *v))
        .collect::<std::collections::BTreeMap<_, _>>();
    pending.extend(
        xs.iter()
            .filter_map(|(k, a)| right.get(k).map(|b| (*a, *b))),
    );
}

// These binder/origin correspondences do not prove a body unchanged. They only
// let imported lexical contexts retain their names and receiving-source origin.
fn context(
    old: &Program,
    new: &Program,
    a: ExprId,
    b: ExprId,
    mapping: &mut Correspondence,
    remaining: &mut usize,
) -> bool {
    if mapping
        .contexts
        .get(&a)
        .is_some_and(|previous| *previous != b)
    {
        return false;
    }
    let (Some(x), Some(y)) = (old.expressions.get(a.0), new.expressions.get(b.0)) else {
        return false;
    };
    let mut locals = Vec::new();
    match (&x.kind, &y.kind) {
        (Kind::Lam(x, _), Kind::Lam(y, _)) => locals.push((*x, *y)),
        (Kind::Let(xs, _), Kind::Let(ys, _)) if xs.len() == ys.len() && xs.len() <= *remaining => {
            locals.extend(xs.iter().zip(ys).map(|((x, _), (y, _))| (*x, *y)));
        }
        (Kind::Case(_, xs), Kind::Case(_, ys))
            if xs.len() == ys.len() && xs.len() <= *remaining =>
        {
            for ((x, _), (y, _)) in xs.iter().zip(ys) {
                if !pattern_context(x, y, &mut locals, remaining) {
                    return false;
                }
            }
        }
        _ => {}
    }
    let Some(left) = remaining.checked_sub(locals.len()) else {
        *remaining = 0;
        return false;
    };
    *remaining = left;
    if locals
        .iter()
        .any(|(x, y)| mapping.locals.get(x).is_some_and(|previous| previous != y))
    {
        return false;
    }
    mapping.locals.extend(locals);
    mapping.contexts.insert(a, b);
    true
}

pub(super) fn pattern_context(
    a: &Pattern,
    b: &Pattern,
    locals: &mut Vec<(LocalId, LocalId)>,
    remaining: &mut usize,
) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        let Some(left) = remaining.checked_sub(1) else {
            return false;
        };
        *remaining = left;
        match (a, b) {
            (Pattern::Bind(x), Pattern::Bind(y)) => locals.push((*x, *y)),
            (Pattern::As(x, p), Pattern::As(y, q)) => {
                locals.push((*x, *y));
                pending.push((p, q));
            }
            (Pattern::Ctor(x, ps), Pattern::Ctor(y, qs))
                if x == y && ps.len() == qs.len() && ps.len() <= *remaining =>
            {
                pending.extend(ps.iter().zip(qs))
            }
            (Pattern::Tuple(ps), Pattern::Tuple(qs))
                if ps.len() == qs.len() && ps.len() <= *remaining =>
            {
                pending.extend(ps.iter().zip(qs))
            }
            (Pattern::Record(ps), Pattern::Record(qs))
                if ps.len() == qs.len() && ps.len() <= *remaining =>
            {
                *remaining -= ps.len();
                for ((x, p), (y, q)) in ps.iter().zip(qs) {
                    if x != y {
                        return false;
                    }
                    locals.push((*p, *q));
                }
            }
            (Pattern::Wildcard, Pattern::Wildcard) => {}
            (Pattern::Lit(x), Pattern::Lit(y)) if same_literal(x, y) => {}
            _ => return false,
        }
        if pending.len() > *remaining {
            return false;
        }
    }
    true
}
