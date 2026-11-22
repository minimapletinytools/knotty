//! First-order, non-overlapping instance patterns for multi-parameter
//! interfaces. Selection matches all arguments without binding caller types.
use crate::{
    error::TypeErrorKind,
    predicate,
    ty::Structure,
    unify::unify,
    var::{Substitution, TypeVarId},
};
use knot_canonical::ast::{CConstraint, CType, Ref};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationInstance {
    pub targets: Vec<CType>,
    pub context: Vec<CConstraint>,
}

/// Source-order contexts for relations; preserve the established positional
/// order for unary dictionaries (including their derived counterparts).
pub fn ordered_context(targets: &[CType], constraints: &[CConstraint]) -> Vec<CConstraint> {
    let mut result = constraints.to_vec();
    if let [CType::Named(_, args)] = targets {
        result.sort_by_key(|c| {
            args.iter()
                .position(|a| matches!(a, CType::Var(n) if c.type_vars.first() == Some(n)))
                .unwrap_or(usize::MAX)
        });
    }
    let mut unique = Vec::new();
    for constraint in result {
        if !unique.contains(&constraint) {
            unique.push(constraint);
        }
    }
    unique
}

/// Binary dictionaries are owned by the declaring module, with an alpha-
/// normalized pattern as their name. Importing them preserves that identity;
/// neither declaration order nor type-variable spelling changes it.
pub fn dictionary_head(interface: &str, targets: &[CType]) -> Ref {
    if let [CType::Named(head, _)] = targets {
        return head.clone();
    }
    fn normalized(ty: &CType, vars: &mut HashMap<String, String>) -> CType {
        match ty {
            CType::Var(v) => {
                let next = vars.len().to_string();
                CType::Var(vars.entry(v.clone()).or_insert(next).clone())
            }
            CType::Named(r, args) => CType::Named(
                r.clone(),
                args.iter().map(|a| normalized(a, vars)).collect(),
            ),
            other => other.clone(), // invalid patterns are diagnosed before export
        }
    }
    let mut vars = HashMap::new();
    let patterns = targets
        .iter()
        .map(|t| normalized(t, &mut vars))
        .collect::<Vec<_>>();
    Ref::TopLevel(format!("$instance:{interface}:{patterns:?}"))
}

pub fn validate(targets: &[CType], context: &[CConstraint]) -> Result<(), TypeErrorKind> {
    fn scan(ty: &CType, vars: &mut HashMap<String, usize>) -> Option<usize> {
        match ty {
            CType::Var(v) => {
                *vars.entry(v.clone()).or_default() += 1;
                Some(1)
            }
            CType::Named(_, args) => {
                let sizes = args
                    .iter()
                    .map(|a| scan(a, vars))
                    .collect::<Option<Vec<_>>>()?;
                Some(1 + sizes.into_iter().sum::<usize>())
            }
            _ => None,
        }
    }
    let bad = |reason: &str| TypeErrorKind::UnsupportedInstanceHead {
        reason: reason.into(),
    };
    let mut vars = HashMap::new();
    let size = targets.iter().map(|t| scan(t,&mut vars)).collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("relation instance patterns must use type variables and nominal constructors; custom structural and higher-kinded patterns are unsupported"))?
        .into_iter().sum::<usize>();
    for c in context {
        let mut counts = HashMap::<&String, usize>::new();
        for v in &c.type_vars {
            *counts.entry(v).or_default() += 1;
        }
        if counts
            .iter()
            .any(|(v, n)| vars.get(*v).is_none_or(|m| n > m))
            || c.type_vars.len() >= size
        {
            return Err(bad("instance contexts must be smaller than the head, and may not introduce or repeat variables more often than the head (to keep instance resolution terminating)"));
        }
    }
    Ok(())
}

fn build(sub: &mut Substitution, ty: &CType, vars: &mut HashMap<String, TypeVarId>) -> TypeVarId {
    match ty {
        CType::Var(v) => *vars.entry(v.clone()).or_insert_with(|| sub.fresh_unbound()),
        CType::Named(r, args) => {
            let args = args.iter().map(|a| build(sub, a, vars)).collect();
            sub.fresh_bound(Structure::App(r.clone(), args))
        }
        _ => unreachable!("validated first-order instance pattern"),
    }
}

/// Existential overlap test with fresh variables on each side and an occurs
/// check. Thus `Constriction t t` and `Constriction (Maybe t) t` are disjoint.
pub fn overlaps(a: &[CType], b: &[CType]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut sub = Substitution::new();
    let mut av = HashMap::new();
    let mut bv = HashMap::new();
    let aa = a.iter().map(|t| build(&mut sub, t, &mut av)).collect();
    let bb = b.iter().map(|t| build(&mut sub, t, &mut bv)).collect();
    let aa = predicate::pack(&mut sub, aa);
    let bb = predicate::pack(&mut sub, bb);
    unify(&mut sub, aa, bb).is_ok()
}

pub fn matches(
    sub: &mut Substitution,
    patterns: &[CType],
    args: &[TypeVarId],
) -> Option<HashMap<String, TypeVarId>> {
    fn go(
        sub: &mut Substitution,
        pattern: &CType,
        ty: TypeVarId,
        vars: &mut HashMap<String, TypeVarId>,
    ) -> bool {
        match pattern {
            CType::Var(v) => match vars.get(v) {
                Some(previous) => predicate::same(sub, *previous, ty),
                None => {
                    vars.insert(v.clone(), ty);
                    true
                }
            },
            CType::Named(r, ps) => match sub.head_normal(ty) {
                Some(Structure::App(s, args) | Structure::Ctor(s, args))
                    if *r == s && ps.len() == args.len() =>
                {
                    ps.iter().zip(args).all(|(p, a)| go(sub, p, a, vars))
                }
                _ => false,
            },
            _ => false,
        }
    }
    let mut vars = HashMap::new();
    (patterns.len() == args.len()
        && patterns
            .iter()
            .zip(args)
            .all(|(p, &a)| go(sub, p, a, &mut vars)))
    .then_some(vars)
}
