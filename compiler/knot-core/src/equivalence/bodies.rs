//! Exact body identity, independently of the versions of referenced globals.
//! This deliberately cannot be passed to the static result-transfer API.
use super::*;
mod fragments;

/// Code-only correspondence between checked versions. A matching body can read
/// changed globals or host inputs, so this is NOT evidence of an unchanged value.
/// The runtime must separately validate demanded reads and map lazy/source edges.
/// The typed embedding must establish the nominal/dictionary/semantic ABI first.
pub struct BodyVersions<'a> {
    old: &'a Program,
    new: &'a Program,
    globals: BTreeSet<Global>,
    expressions: HashMap<ExprId, ExprId>,
    locals: HashMap<LocalId, LocalId>,
}
impl BodyVersions<'_> {
    /// Arena mappings apply only to this exact immutable source/target pair.
    pub fn accepts(&self, old: &Program, new: &Program) -> bool {
        std::ptr::eq(self.old, old) && std::ptr::eq(self.new, new)
    }
    pub fn contains(&self, global: &Global) -> bool {
        self.globals.contains(global)
    }
    pub fn expression(&self, old: ExprId) -> Option<ExprId> {
        self.expressions.get(&old).copied()
    }
    pub fn local(&self, old: LocalId) -> Option<LocalId> {
        self.locals.get(&old).copied()
    }
    pub fn expressions(&self) -> impl Iterator<Item = (ExprId, ExprId)> + '_ {
        self.expressions.iter().map(|(old, new)| (*old, *new))
    }
    pub fn locals(&self) -> impl Iterator<Item = (LocalId, LocalId)> + '_ {
        self.locals.iter().map(|(old, new)| (*old, *new))
    }
    pub fn globals(&self) -> impl Iterator<Item = &Global> {
        self.globals.iter()
    }
}

/// Compare whole bodies without following global references. Names, literal bits,
/// dictionaries, branches and host signatures still match exactly after arena/
/// local renumbering. Host versions and global results are runtime dependencies.
/// Exhaustion keeps only fully checked bodies. Conflicting correspondence rejects
/// the entire proposal; no ambiguous earlier mapping remains authoritative.
pub fn body_versions<'a>(
    old: &'a Program,
    new: &'a Program,
    roots: &[Global],
    remaining: &mut usize,
) -> Option<BodyVersions<'a>> {
    let mut result = BodyVersions {
        old,
        new,
        globals: Default::default(),
        expressions: Default::default(),
        locals: Default::default(),
    };
    let mut expressions_back = HashMap::new();
    let mut locals_back = HashMap::new();
    for global in roots {
        let Some(left) = remaining.checked_sub(1) else {
            break;
        };
        *remaining = left;
        if result.globals.contains(global) {
            continue;
        }
        let Some(matched) = compare_body(old, new, global, remaining) else {
            break;
        };
        let Some(matched) = matched else { continue };
        for (a, b) in matched.expressions {
            pair(&mut result.expressions, &mut expressions_back, a, b)?;
        }
        for (a, b) in matched.locals {
            pair(&mut result.locals, &mut locals_back, a, b)?;
        }
        result.globals.insert(global.clone());
    }
    Some(result)
}
fn pair<T: Copy + Eq + std::hash::Hash>(
    forward: &mut HashMap<T, T>,
    backward: &mut HashMap<T, T>,
    a: T,
    b: T,
) -> Option<()> {
    if forward.insert(a, b).is_some_and(|previous| previous != b)
        || backward.insert(b, a).is_some_and(|previous| previous != a)
    {
        return None;
    }
    Some(())
}

/// Whole bodies plus exact fragments within structurally aligned changed bodies.
/// Local-role correspondence is not source-scope compatibility: runtimes must
/// validate the full receiving lexical provenance before borrowing cached values.
/// Fragment identity also does not prove the values of referenced globals stable.
/// Exhaustion keeps complete proofs; conflicting correspondence rejects all.
pub fn source_versions<'a>(
    old: &'a Program,
    new: &'a Program,
    roots: &[Global],
    remaining: &mut usize,
) -> Option<BodyVersions<'a>> {
    let mut result = body_versions(old, new, roots, remaining)?;
    let scratch = result.expressions.len().checked_add(result.locals.len())?;
    if scratch <= *remaining {
        fragments::extend(&mut result, roots, remaining)?;
    }
    Some(result)
}
