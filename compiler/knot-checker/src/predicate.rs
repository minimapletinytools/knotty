//! Multi-parameter dictionary arguments share the type arena with ordinary
//! types so substitution, levels, copying and interface fingerprints traverse
//! every argument. `Predicate` is never unified with a source-level type.

use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};

pub fn pack(sub: &mut Substitution, args: Vec<TypeVarId>) -> TypeVarId {
    if args.len() == 1 {
        args[0]
    } else {
        sub.fresh_bound(Structure::Predicate(args))
    }
}

/// Definitional equality only: dictionary lookup must never refine the types
/// of an expression to make an instance fit.
pub fn same(sub: &mut Substitution, a: TypeVarId, b: TypeVarId) -> bool {
    let a = sub.find(a);
    let b = sub.find(b);
    if a == b {
        return true;
    }
    match (sub.head_normal(a), sub.head_normal(b)) {
        (
            Some(Structure::App(r, xs) | Structure::Ctor(r, xs)),
            Some(Structure::App(s, ys) | Structure::Ctor(s, ys)),
        ) if r == s => same_args(sub, &xs, &ys),
        (Some(Structure::Predicate(xs)), Some(Structure::Predicate(ys)))
        | (Some(Structure::Tuple(xs)), Some(Structure::Tuple(ys))) => same_args(sub, &xs, &ys),
        (Some(Structure::Fn(a, b)), Some(Structure::Fn(c, d))) => {
            same(sub, a, c) && same(sub, b, d)
        }
        (Some(Structure::VarApp(f, xs)), Some(Structure::VarApp(g, ys))) => {
            same(sub, f, g) && same_args(sub, &xs, &ys)
        }
        (Some(Structure::Unit), Some(Structure::Unit)) => true,
        (Some(Structure::Record(fs, x)), Some(Structure::Record(gs, y))) => {
            let (fs, x) = crate::interface::instance::flatten_record(sub, fs, x);
            let (gs, y) = crate::interface::instance::flatten_record(sub, gs, y);
            fs.keys().eq(gs.keys())
                && fs.values().zip(gs.values()).all(|(&a, &b)| same(sub, a, b))
                && match (x, y) {
                    (None, None) => true,
                    (Some(a), Some(b)) => same(sub, a, b),
                    _ => false,
                }
        }
        _ => false,
    }
}

fn same_args(sub: &mut Substitution, xs: &[TypeVarId], ys: &[TypeVarId]) -> bool {
    xs.len() == ys.len() && xs.iter().zip(ys).all(|(&a, &b)| same(sub, a, b))
}
