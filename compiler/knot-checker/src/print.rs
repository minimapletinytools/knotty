//! Rendering types and schemes as Knot source text — for error messages and
//! for the corpus's inferred-type golden tests.
//!
//! Flexible variables are named `a`, `b`, `c`, ... in order of first
//! appearance; rigid variables keep their signature's own name. A scheme's
//! constraints are printed after superclass simplification (`Ord a`
//! subsumes `Eq a`) and sorted by variable, then interface name, so the
//! output for a given type is deterministic.

use std::collections::{BTreeMap, HashMap, HashSet};

use knot_canonical::ast::Ref;

use crate::interface::table::superclasses;
use crate::ty::{Scheme, Structure};
use crate::var::{Substitution, TypeVarId};

#[derive(Default)]
pub struct Printer {
    names: HashMap<TypeVarId, String>,
    order: Vec<TypeVarId>,
    next: usize,
}

pub fn ref_name(r: &Ref) -> String {
    match r {
        Ref::Local(n) | Ref::TopLevel(n) | Ref::Builtin(n) | Ref::Unresolved(n) => n.clone(),
        Ref::Imported { module, name } => format!("{}.{}", module.join("."), name),
    }
}

impl Printer {
    pub fn new() -> Self {
        Printer::default()
    }

    fn var_name(&mut self, sub: &mut Substitution, root: TypeVarId) -> String {
        if let Some(name) = self.names.get(&root) {
            return name.clone();
        }
        let name = match sub.rigid_name(root) {
            Some(n) => n,
            None => loop {
                let candidate = generated_name(self.next);
                self.next += 1;
                if !self.names.values().any(|n| *n == candidate) {
                    break candidate;
                }
            },
        };
        self.names.insert(root, name.clone());
        self.order.push(root);
        name
    }

    pub fn ty(&mut self, sub: &mut Substitution, ty: TypeVarId) -> String {
        self.prec(sub, ty, 0)
    }

    /// `prec`: 0 = anything, 1 = argument of an arrow's left side (no bare
    /// arrows), 2 = argument of an application (atoms only).
    fn prec(&mut self, sub: &mut Substitution, ty: TypeVarId, prec: u8) -> String {
        let root = sub.find(ty);
        let Some(structure) = sub.head_normal(root) else {
            return self.var_name(sub, root);
        };
        match structure {
            Structure::Unit => "()".to_string(),
            Structure::App(r, args) | Structure::Ctor(r, args) => {
                if args.is_empty() {
                    return ref_name(&r);
                }
                let mut s = ref_name(&r);
                for a in args {
                    s.push(' ');
                    s.push_str(&self.prec(sub, a, 2));
                }
                paren(s, prec >= 2)
            }
            Structure::VarApp(f, args) => {
                let mut s = self.prec(sub, f, 2);
                for a in args {
                    s.push(' ');
                    s.push_str(&self.prec(sub, a, 2));
                }
                paren(s, prec >= 2)
            }
            Structure::Fn(a, b) => {
                let s = format!("{} -> {}", self.prec(sub, a, 1), self.prec(sub, b, 0));
                paren(s, prec >= 1)
            }
            Structure::Predicate(elems) => elems
                .into_iter()
                .map(|e| self.prec(sub, e, 2))
                .collect::<Vec<_>>()
                .join(" "),
            Structure::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|e| self.prec(sub, *e, 0)).collect();
                format!("({})", parts.join(", "))
            }
            Structure::Record(fields, ext) => {
                let (fields, tail) = flatten_record(sub, fields, ext);
                let body: Vec<String> = fields
                    .iter()
                    .map(|(n, t)| format!("{n} : {}", self.prec(sub, *t, 0)))
                    .collect();
                match tail {
                    None if body.is_empty() => "{}".to_string(),
                    None => format!("{{ {} }}", body.join(", ")),
                    Some(r) => {
                        let r = self.var_name(sub, r);
                        if body.is_empty() {
                            format!("{{ {r} | }}")
                        } else {
                            format!("{{ {r} | {} }}", body.join(", "))
                        }
                    }
                }
            }
        }
    }

    pub fn scheme(&mut self, sub: &mut Substitution, scheme: &Scheme) -> String {
        let ty = self.ty(sub, scheme.ty);
        let constraints = simplify_constraints(sub, &scheme.constraints);
        if constraints.is_empty() {
            return ty;
        }
        let position: HashMap<TypeVarId, usize> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, v)| (*v, i))
            .collect();
        let mut rendered: Vec<(usize, String, String)> = constraints
            .into_iter()
            .map(|(v, iface)| {
                let root = sub.find(v);
                let pos = position.get(&root).copied().unwrap_or(usize::MAX);
                let name = self.prec(sub, root, 2);
                (pos, iface, name)
            })
            .collect();
        rendered.sort();
        rendered.dedup();
        let parts: Vec<String> = rendered
            .into_iter()
            .map(|(_, iface, name)| format!("{iface} {name}"))
            .collect();
        if parts.len() == 1 {
            format!("{} => {ty}", parts[0])
        } else {
            format!("({}) => {ty}", parts.join(", "))
        }
    }
}

fn paren(s: String, wrap: bool) -> String {
    if wrap {
        format!("({s})")
    } else {
        s
    }
}

fn generated_name(n: usize) -> String {
    let letter = (b'a' + (n % 26) as u8) as char;
    if n < 26 {
        letter.to_string()
    } else {
        format!("{letter}{}", n / 26)
    }
}

/// Follows a record's extension chain, merging any fields bound into it.
fn flatten_record(
    sub: &mut Substitution,
    mut fields: BTreeMap<String, TypeVarId>,
    mut ext: Option<TypeVarId>,
) -> (BTreeMap<String, TypeVarId>, Option<TypeVarId>) {
    while let Some(e) = ext {
        match sub.resolve_structure(e) {
            Some(Structure::Record(more, next)) => {
                fields.extend(more);
                ext = next;
            }
            _ => return (fields, Some(sub.find(e))),
        }
    }
    (fields, None)
}

/// Drops any constraint implied by a superclass of another constraint on the
/// same variable (`(Eq a, Ord a)` becomes `Ord a`).
pub fn simplify_constraints(
    sub: &mut Substitution,
    constraints: &[(TypeVarId, String)],
) -> Vec<(TypeVarId, String)> {
    let mut by_var: Vec<(TypeVarId, HashSet<String>)> = Vec::new();
    for (v, iface) in constraints {
        let root = sub.find(*v);
        match by_var
            .iter_mut()
            .find(|(r, _)| crate::predicate::same(sub, *r, root))
        {
            Some((_, set)) => {
                set.insert(iface.clone());
            }
            None => by_var.push((root, HashSet::from([iface.clone()]))),
        }
    }
    let mut out = Vec::new();
    for (v, set) in by_var {
        let implied: HashSet<String> = set.iter().flat_map(|i| all_superclasses(i)).collect();
        let mut kept: Vec<String> = set.into_iter().filter(|i| !implied.contains(i)).collect();
        kept.sort();
        out.extend(kept.into_iter().map(|i| (v, i)));
    }
    out
}

pub fn all_superclasses(interface: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<String> = superclasses(interface)
        .iter()
        .map(|s| s.to_string())
        .collect();
    while let Some(s) = stack.pop() {
        if out.insert(s.clone()) {
            stack.extend(superclasses(&s).iter().map(|x| x.to_string()));
        }
    }
    out
}

/// One-shot convenience for a single type.
pub fn print_type(sub: &mut Substitution, ty: TypeVarId) -> String {
    Printer::new().ty(sub, ty)
}

pub fn print_scheme(sub: &mut Substitution, scheme: &Scheme) -> String {
    Printer::new().scheme(sub, scheme)
}
