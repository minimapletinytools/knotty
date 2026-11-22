//! Building checker types from the prelude's parsed signatures
//! (`knot_syntax::ast::ty::Type`). Every capitalized name in the prelude
//! is a builtin type: `Ref::Builtin`, or `Ref::Imported` for a qualified
//! module's type like `Map.Map` (`builtin_type_ref`). How a type variable is
//! materialized is the caller's choice (`VarMaker`): fresh flexible
//! variables for a prelude scheme, fresh rigid ones when checking an
//! instance method against its interface's signature.

use std::collections::HashMap;

use knot_canonical::ast::Ref;
use knot_canonical::prelude::builtin_type_ref;
use knot_syntax::ast::ty::Type;

use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};

/// What a type-variable name stands for while building one signature.
#[derive(Debug, Clone)]
pub enum VarBinding {
    /// An ordinary variable (or a constructor variable used as `f a`).
    Ty(TypeVarId),
    /// An interface's own constructor variable, substituted by a concrete
    /// (possibly partially applied) constructor — `f` in `Functor f`, for
    /// `instance Functor (Outcome e)`, becomes `Outcome e`.
    Ctor(Ref, Vec<TypeVarId>),
}

pub type VarMaker<'a> = dyn FnMut(&mut Substitution, &str) -> TypeVarId + 'a;

pub fn build(
    sub: &mut Substitution,
    ty: &Type,
    vars: &mut HashMap<String, VarBinding>,
    make: &mut VarMaker,
) -> TypeVarId {
    match ty {
        // `Unit` is another name for `()`.
        Type::Named(name, args) if name == "Unit" && args.is_empty() => {
            sub.fresh_bound(Structure::Unit)
        }
        Type::Named(name, args) => {
            let args = args.iter().map(|a| build(sub, a, vars, make)).collect();
            sub.fresh_bound(Structure::App(builtin_type_ref(name), args))
        }
        Type::Var(name) => match lookup(sub, name, vars, make) {
            VarBinding::Ty(id) => id,
            VarBinding::Ctor(r, leading) => sub.fresh_bound(Structure::App(r, leading)),
        },
        Type::VarApp(name, args) => {
            let args: Vec<TypeVarId> = args.iter().map(|a| build(sub, a, vars, make)).collect();
            match lookup(sub, name, vars, make) {
                VarBinding::Ty(f) => sub.fresh_bound(Structure::VarApp(f, args)),
                VarBinding::Ctor(r, mut leading) => {
                    leading.extend(args);
                    sub.fresh_bound(Structure::App(r, leading))
                }
            }
        }
        Type::Fn(a, b) => {
            let a = build(sub, a, vars, make);
            let b = build(sub, b, vars, make);
            sub.fresh_bound(Structure::Fn(a, b))
        }
        Type::Tuple(elems) => {
            let elems = elems.iter().map(|e| build(sub, e, vars, make)).collect();
            sub.fresh_bound(Structure::Tuple(elems))
        }
        Type::Record(fields, ext) => {
            let fields = fields
                .iter()
                .map(|(n, t)| (n.clone(), build(sub, t, vars, make)))
                .collect();
            let ext = ext.as_ref().map(|n| match lookup(sub, n, vars, make) {
                VarBinding::Ty(id) => id,
                VarBinding::Ctor(..) => unreachable!("a row variable is never a constructor"),
            });
            sub.fresh_bound(Structure::Record(fields, ext))
        }
        Type::Unit => sub.fresh_bound(Structure::Unit),
    }
}

fn lookup(
    sub: &mut Substitution,
    name: &str,
    vars: &mut HashMap<String, VarBinding>,
    make: &mut VarMaker,
) -> VarBinding {
    if let Some(b) = vars.get(name) {
        return b.clone();
    }
    let id = make(sub, name);
    vars.insert(name.to_string(), VarBinding::Ty(id));
    VarBinding::Ty(id)
}

/// The variable a name was bound to, if it's an ordinary one.
pub fn var_id(vars: &HashMap<String, VarBinding>, name: &str) -> Option<TypeVarId> {
    match vars.get(name)? {
        VarBinding::Ty(id) => Some(*id),
        VarBinding::Ctor(..) => None,
    }
}

/// Build an obligation from the variables bound while reading a signature.
pub(crate) fn constraint_args(
    sub: &mut Substitution,
    vars: &HashMap<String, VarBinding>,
    names: &[String],
) -> Option<TypeVarId> {
    let args = names
        .iter()
        .map(|name| match vars.get(name)? {
            VarBinding::Ty(id) => Some(*id),
            VarBinding::Ctor(r, args) => {
                Some(sub.fresh_bound(Structure::Ctor(r.clone(), args.clone())))
            }
        })
        .collect::<Option<Vec<_>>>()?;
    Some(crate::predicate::pack(sub, args))
}
