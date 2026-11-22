//! Conservative static equality for closed-result reuse. Compare executable
//! graphs exactly after renumbering local/arena IDs; include every branch and
//! transitive global dependency. No source spans or probabilistic hashes.
use crate::*;
use std::collections::{BTreeSet, HashMap};
mod bodies;
pub use bodies::{body_versions, source_versions, BodyVersions};

pub fn unchanged_globals(
    old: &Program,
    new: &Program,
    roots: &[Global],
    max_nodes: usize,
) -> BTreeSet<Global> {
    unchanged_globals_with_budget(old, new, roots, &mut { max_nodes })
}
/// Compare the executable consumers of explicit input globals. The caller must
/// check input ABIs and supply their current values or implementations. Matching
/// consumers authorize reuse of code only, never its previous results.
pub fn same_code_with_inputs(
    old: &Program,
    new: &Program,
    roots: &[Global],
    inputs: &BTreeSet<Global>,
    max_nodes: usize,
) -> bool {
    if roots.iter().any(|r| inputs.contains(r))
        || inputs
            .iter()
            .any(|g| !old.globals.contains_key(g) || !new.globals.contains_key(g))
    {
        return false;
    }
    let matched = compare_roots(
        old,
        new,
        roots,
        &mut { max_nodes },
        &BTreeSet::new(),
        inputs,
    )
    .0;
    roots.iter().all(|root| matched.contains(root))
}

/// Like `unchanged_globals`, charging a caller's cumulative comparison allowance.
/// Exhaustion returns only roots already proved unchanged, never a partial proof.
pub fn unchanged_globals_with_budget(
    old: &Program,
    new: &Program,
    roots: &[Global],
    remaining: &mut usize,
) -> BTreeSet<Global> {
    compare_roots(
        old,
        new,
        roots,
        remaining,
        &BTreeSet::new(),
        &BTreeSet::new(),
    )
    .0
}
struct Match {
    dependencies: BTreeSet<Global>,
    hosts: BTreeSet<GlobalName>,
    expressions: Vec<(ExprId, ExprId)>,
    locals: Vec<(LocalId, LocalId)>,
}
// None means the shared budget ended; Some(None) means changed/unsupported code.
// Static transfer and dynamic code-version preparation share this exact relation.
fn compare_body(
    old: &Program,
    new: &Program,
    global: &Global,
    remaining: &mut usize,
) -> Option<Option<Match>> {
    let (Some(a), Some(b)) = (old.globals.get(global), new.globals.get(global)) else {
        return Some(None);
    };
    let left = shape(old, *a, remaining)?;
    let right = shape(new, *b, remaining)?;
    Some(
        (same_kinds(&left.kinds, &right.kinds)
            && !right
                .kinds
                .iter()
                .any(|kind| matches!(kind, Kind::Unsupported(_))))
        .then(|| Match {
            hosts: right
                .kinds
                .iter()
                .filter_map(|kind| match kind {
                    Kind::Host(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect(),
            dependencies: right.globals,
            expressions: left
                .expressions
                .into_iter()
                .zip(right.expressions)
                .collect(),
            locals: left.locals.into_iter().zip(right.locals).collect(),
        }),
    )
}
type Compared = HashMap<Global, Option<Match>>;
fn compare_roots(
    old: &Program,
    new: &Program,
    roots: &[Global],
    remaining: &mut usize,
    compatible_hosts: &BTreeSet<GlobalName>,
    inputs: &BTreeSet<Global>,
) -> (BTreeSet<Global>, Compared) {
    let mut unchanged = BTreeSet::new();
    let mut compared = Compared::new();
    for root in roots {
        if unchanged.contains(root) {
            continue;
        }
        let mut todo = vec![root.clone()];
        let mut visited = BTreeSet::new();
        let mut valid = true;
        while let Some(global) = todo.pop() {
            if inputs.contains(&global)
                || unchanged.contains(&global)
                || !visited.insert(global.clone())
            {
                continue;
            }
            if !compared.contains_key(&global) {
                let Some(equal) = compare_body(old, new, &global, remaining) else {
                    return (unchanged, compared);
                };
                let equal = equal.filter(|matched| matched.hosts.is_subset(compatible_hosts));
                compared.insert(global.clone(), equal);
            }
            match &compared[&global] {
                Some(matched) => todo.extend(matched.dependencies.iter().rev().cloned()),
                None => {
                    valid = false;
                    break;
                }
            }
        }
        // A cycle is accepted only after its entire reachable graph has been
        // checked. Encountering a node on the active path alone proves nothing.
        if valid {
            unchanged.extend(visited);
        }
    }
    (unchanged, compared)
}

/// Exact code correspondences for transferring session-owned lazy graphs.
/// Absence is a conservative cache miss, including ambiguous arena mappings.
pub struct Correspondence {
    pub globals: BTreeSet<Global>,
    pub expressions: HashMap<ExprId, ExprId>,
    pub locals: HashMap<LocalId, LocalId>,
    /// Debug/lexical context alignment only; NEVER authorizes executable reuse.
    pub contexts: HashMap<ExprId, ExprId>,
    /// Checked fragments within changed definitions; not whole unchanged globals.
    pub fragments: BTreeSet<ExprId>,
}
struct Shape {
    kinds: Vec<Kind>,
    globals: BTreeSet<Global>,
    expressions: Vec<ExprId>,
    locals: Vec<LocalId>,
    closed: bool,
}
pub fn correspondence(
    old: &Program,
    new: &Program,
    roots: &[Global],
    max_nodes: usize,
) -> Option<Correspondence> {
    correspondence_with_hosts(old, new, roots, max_nodes, &BTreeSet::new())
}

/// The embedding must verify identical signatures, implementation/input versions
/// and native Rust type registrations before opting a host function into reuse.
/// Core still compares every expression and transitive dependency exactly.
pub fn correspondence_with_hosts(
    old: &Program,
    new: &Program,
    roots: &[Global],
    max_nodes: usize,
    compatible_hosts: &BTreeSet<GlobalName>,
) -> Option<Correspondence> {
    correspondence_with_closed_expressions(old, new, roots, &[], max_nodes, compatible_hosts)
}
/// Also match completed, capture-free computations inside changed roots.
/// Every accepted fragment has exact executable shape and already-proved global
/// dependencies. Alignment only proposes candidates; it is never the proof.
pub fn correspondence_with_closed_expressions(
    old: &Program,
    new: &Program,
    roots: &[Global],
    completed: &[ExprId],
    max_nodes: usize,
    compatible_hosts: &BTreeSet<GlobalName>,
) -> Option<Correspondence> {
    correspondence_impl(
        old,
        new,
        roots,
        completed,
        &mut { max_nodes },
        compatible_hosts,
        false,
    )
}
/// Open fragments require the runtime to compare every captured lazy cell.
/// Charges the caller's shared code/observation allowance.
pub fn correspondence_with_observed_expressions(
    old: &Program,
    new: &Program,
    roots: &[Global],
    completed: &[ExprId],
    remaining: &mut usize,
    compatible_hosts: &BTreeSet<GlobalName>,
) -> Option<Correspondence> {
    correspondence_impl(
        old,
        new,
        roots,
        completed,
        remaining,
        compatible_hosts,
        true,
    )
}
fn correspondence_impl(
    old: &Program,
    new: &Program,
    roots: &[Global],
    completed: &[ExprId],
    remaining: &mut usize,
    compatible_hosts: &BTreeSet<GlobalName>,
    captures: bool,
) -> Option<Correspondence> {
    let (globals, compared) = compare_roots(
        old,
        new,
        roots,
        remaining,
        compatible_hosts,
        &BTreeSet::new(),
    );
    let mut result = Correspondence {
        globals,
        expressions: HashMap::new(),
        locals: HashMap::new(),
        contexts: HashMap::new(),
        fragments: BTreeSet::new(),
    };
    for global in &result.globals {
        let matched = compared.get(global)?.as_ref()?;
        for (a, b) in matched.expressions.iter().copied() {
            if result
                .expressions
                .insert(a, b)
                .is_some_and(|previous| previous != b)
            {
                return None;
            }
        }
        for (a, b) in matched.locals.iter().copied() {
            if result
                .locals
                .insert(a, b)
                .is_some_and(|previous| previous != b)
            {
                return None;
            }
        }
    }
    fragments::extend(
        old,
        new,
        roots,
        fragments::Observed {
            expressions: completed,
            captures,
        },
        &mut result,
        remaining,
        compatible_hosts,
    );
    Some(result)
}
mod fragments;

fn same_literal(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::Float(a), Literal::Float(b)) => a.to_bits() == b.to_bits(),
        _ => a == b,
    }
}
fn same_pattern(a: &Pattern, b: &Pattern) -> bool {
    match (a, b) {
        (Pattern::Lit(a), Pattern::Lit(b)) => same_literal(a, b),
        (Pattern::As(a, x), Pattern::As(b, y)) => a == b && same_pattern(x, y),
        (Pattern::Ctor(a, xs), Pattern::Ctor(b, ys)) => a == b && same_patterns(xs, ys),
        (Pattern::Tuple(xs), Pattern::Tuple(ys)) => same_patterns(xs, ys),
        _ => a == b,
    }
}
fn same_patterns(a: &[Pattern], b: &[Pattern]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_pattern(a, b))
}
fn same_kinds(a: &[Kind], b: &[Kind]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| match (a, b) {
            (Kind::Lit(a), Kind::Lit(b)) => same_literal(a, b),
            (Kind::Case(a, xs), Kind::Case(b, ys)) => {
                a == b
                    && xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys)
                        .all(|((a, x), (b, y))| x == y && same_pattern(a, b))
            }
            _ => a == b,
        })
}

fn shape(program: &Program, root: ExprId, remaining: &mut usize) -> Option<Shape> {
    shape_with_globals(program, root, remaining, None)
}
fn shape_with_globals(
    program: &Program,
    root: ExprId,
    remaining: &mut usize,
    allowed: Option<&BTreeSet<Global>>,
) -> Option<Shape> {
    let mut todo = vec![root];
    let mut ids = HashMap::from([(root, ExprId(0))]);
    let mut locals = HashMap::new();
    let mut kinds = Vec::new();
    let mut globals = BTreeSet::new();
    let mut bound = BTreeSet::new();
    let mut used = BTreeSet::new();
    let mut cursor = 0;
    while cursor < todo.len() {
        *remaining = remaining.checked_sub(1)?;
        let mut kind = program.expressions.get(todo[cursor].0)?.kind.clone();
        cursor += 1;
        // Whole-global equality already has a closed root. Only fragment
        // candidates need the additional free-variable check.
        if allowed.is_some() {
            match &kind {
                Kind::Local(id) => {
                    used.insert(*id);
                }
                Kind::Lam(id, _) => {
                    bound.insert(*id);
                }
                Kind::Let(bindings, _) => bound.extend(bindings.iter().map(|(id, _)| *id)),
                Kind::Case(_, arms) => {
                    for (p, _) in arms {
                        pattern(&mut p.clone(), &mut |id| {
                            bound.insert(*id);
                        });
                    }
                }
                _ => {}
            }
        }
        let mut edge = |id: &mut ExprId| {
            *id = *ids.entry(*id).or_insert_with(|| {
                let next = ExprId(todo.len());
                todo.push(*id);
                next
            });
        };
        let mut local = |id: &mut LocalId| {
            let next = LocalId(locals.len());
            *id = *locals.entry(*id).or_insert(next);
        };
        match &mut kind {
            Kind::Global(g) => {
                if allowed.is_some_and(|globals| !globals.contains(g)) {
                    return None;
                }
                globals.insert(g.clone());
            }
            Kind::Local(l) => local(l),
            Kind::Lam(l, e) => {
                local(l);
                edge(e);
            }
            Kind::Select(e, _) => edge(e),
            Kind::App(f, args) => {
                edge(f);
                for a in args {
                    edge(a);
                }
            }
            Kind::Let(bs, b) => {
                for (l, e) in bs {
                    local(l);
                    edge(e);
                }
                edge(b);
            }
            Kind::Case(s, arms) => {
                edge(s);
                for (p, e) in arms {
                    pattern(p, &mut local);
                    edge(e);
                }
            }
            Kind::Tuple(xs) => {
                for e in xs {
                    edge(e);
                }
            }
            Kind::Record(fs) => {
                for (_, e) in fs {
                    edge(e);
                }
            }
            Kind::Dict(fs) => {
                for (_, e) in fs {
                    edge(e);
                }
            }
            Kind::RecordUpdate(b, fs) => {
                edge(b);
                for (_, e) in fs {
                    edge(e);
                }
            }
            _ => {}
        }
        kinds.push(kind);
    }
    let mut local_ids = vec![LocalId(0); locals.len()];
    for (original, normalized) in locals {
        local_ids[normalized.0] = original;
    }
    Some(Shape {
        kinds,
        globals,
        expressions: todo,
        locals: local_ids,
        closed: used.is_subset(&bound),
    })
}
fn pattern(p: &mut Pattern, local: &mut impl FnMut(&mut LocalId)) {
    match p {
        Pattern::Bind(l) => local(l),
        Pattern::As(l, p) => {
            local(l);
            pattern(p, local);
        }
        Pattern::Ctor(_, ps) | Pattern::Tuple(ps) => {
            for p in ps {
                pattern(p, local);
            }
        }
        Pattern::Record(fs) => {
            for (_, l) in fs {
                local(l);
            }
        }
        _ => {}
    }
}
