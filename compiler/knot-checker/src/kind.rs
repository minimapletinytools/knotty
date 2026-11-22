//! Kind checking: every type expression must be well-formed before its
//! values are type-checked. Kinds are `Type` (the kind of values' types),
//! `Row` (a record's extension variable), and `k -> k` (a type constructor
//! awaiting arguments).
//!
//! Builtin types take `Type` arguments (`Map : Type -> Type -> Type`). A
//! user `type` declaration's parameters are inferred from how its
//! constructors' fields use them (`type Wrap f a = Wrap (f a)` gives `f :
//! Type -> Type`), all declarations together so mutual recursion works;
//! anything left undetermined defaults to `Type`. Signatures, aliases, and
//! instance heads are then checked against those kinds: an interface's own
//! variable has kind `Type` (`Eq a`) or `Type -> Type` (`Functor f`).

use std::collections::HashMap;

use knot_canonical::ast::{CConstraint, CDecl, CType, CTypeSignature, Ref};
use knot_canonical::prelude::prelude;
use knot_syntax::span::{Span, Spanned};

use crate::error::{TypeError, TypeErrorKind};
use crate::interface::table::{interface, parameter_arity};

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Type,
    Row,
    Arrow(Box<Kind>, Box<Kind>),
    Var(usize),
}

fn arrows(n: usize) -> Kind {
    (0..n).fold(Kind::Type, |acc, _| {
        Kind::Arrow(Box::new(Kind::Type), Box::new(acc))
    })
}

#[derive(Default)]
struct Kinds {
    slots: Vec<Option<Kind>>,
    env: HashMap<Ref, Kind>,
    errors: Vec<TypeError>,
}

impl Kinds {
    fn fresh(&mut self) -> Kind {
        self.slots.push(None);
        Kind::Var(self.slots.len() - 1)
    }

    fn resolve(&self, k: &Kind) -> Kind {
        match k {
            Kind::Var(v) => match &self.slots[*v] {
                Some(inner) => self.resolve(inner),
                None => k.clone(),
            },
            Kind::Arrow(a, b) => Kind::Arrow(Box::new(self.resolve(a)), Box::new(self.resolve(b))),
            other => other.clone(),
        }
    }

    fn occurs(&self, v: usize, k: &Kind) -> bool {
        match self.resolve(k) {
            Kind::Var(w) => v == w,
            Kind::Arrow(a, b) => self.occurs(v, &a) || self.occurs(v, &b),
            _ => false,
        }
    }

    fn unify(&mut self, a: &Kind, b: &Kind) -> bool {
        match (self.resolve(a), self.resolve(b)) {
            (Kind::Var(x), Kind::Var(y)) if x == y => true,
            (Kind::Var(x), other) | (other, Kind::Var(x)) => {
                if self.occurs(x, &other) {
                    return false;
                }
                self.slots[x] = Some(other);
                true
            }
            (Kind::Arrow(a1, b1), Kind::Arrow(a2, b2)) => {
                self.unify(&a1, &a2) && self.unify(&b1, &b2)
            }
            (x, y) => x == y,
        }
    }

    fn show(&self, k: &Kind) -> String {
        match self.resolve(k) {
            Kind::Type | Kind::Var(_) => "Type".to_string(),
            Kind::Row => "Row".to_string(),
            Kind::Arrow(a, b) => {
                let left = match self.resolve(&a) {
                    Kind::Arrow(..) => format!("({})", self.show(&a)),
                    _ => self.show(&a),
                };
                format!("{left} -> {}", self.show(&b))
            }
        }
    }

    fn expect(&mut self, span: Span, actual: &Kind, expected: &Kind, context: &str) {
        if !self.unify(actual, expected) {
            let kind = TypeErrorKind::KindMismatch {
                expected: self.show(expected),
                actual: self.show(actual),
                context: context.to_string(),
            };
            self.errors.push(TypeError { span, kind });
        }
    }

    /// Defaults every still-unknown kind to `Type`.
    fn default_all(&mut self) {
        for slot in &mut self.slots {
            if slot.is_none() {
                *slot = Some(Kind::Type);
            }
        }
    }

    fn infer(
        &mut self,
        span: Span,
        ty: &CType,
        vars: &mut HashMap<String, Kind>,
        context: &str,
    ) -> Kind {
        match ty {
            CType::Named(r, args) => {
                let head = match self.env.get(r) {
                    Some(k) => k.clone(),
                    // Unknown (unresolved or imported): already reported
                    // elsewhere, or not checkable here — accept any kind.
                    None => self.fresh(),
                };
                self.apply(span, head, args, vars, context)
            }
            CType::Var(name) => self.var(name, vars),
            CType::VarApp(name, args) => {
                let head = self.var(name, vars);
                self.apply(span, head, args, vars, context)
            }
            CType::Fn(a, b) => {
                self.check_type(span, a, vars, context);
                self.check_type(span, b, vars, context);
                Kind::Type
            }
            CType::Tuple(elems) => {
                for e in elems {
                    self.check_type(span, e, vars, context);
                }
                Kind::Type
            }
            CType::Record(fields, ext) => {
                for (_, t) in fields {
                    self.check_type(span, t, vars, context);
                }
                if let Some(r) = ext {
                    let k = self.var(r, vars);
                    self.expect(span, &k, &Kind::Row, context);
                }
                Kind::Type
            }
            CType::Unit => Kind::Type,
        }
    }

    fn var(&mut self, name: &str, vars: &mut HashMap<String, Kind>) -> Kind {
        if let Some(k) = vars.get(name) {
            return k.clone();
        }
        let k = self.fresh();
        vars.insert(name.to_string(), k.clone());
        k
    }

    fn apply(
        &mut self,
        span: Span,
        head: Kind,
        args: &[CType],
        vars: &mut HashMap<String, Kind>,
        context: &str,
    ) -> Kind {
        let mut kind = head;
        for arg in args {
            let arg_kind = self.infer(span, arg, vars, context);
            let result = self.fresh();
            let wanted = Kind::Arrow(Box::new(arg_kind), Box::new(result.clone()));
            if !self.unify(&kind, &wanted) {
                let kind_str = self.show(&kind);
                let error = TypeErrorKind::KindMismatch {
                    expected: self.show(&wanted),
                    actual: kind_str,
                    context: context.to_string(),
                };
                self.errors.push(TypeError { span, kind: error });
                return self.fresh();
            }
            kind = result;
        }
        kind
    }

    fn check_type(
        &mut self,
        span: Span,
        ty: &CType,
        vars: &mut HashMap<String, Kind>,
        context: &str,
    ) {
        let k = self.infer(span, ty, vars, context);
        self.expect(span, &k, &Kind::Type, context);
    }

    fn check_signature(&mut self, name: &str, sig: &Spanned<CTypeSignature>) {
        let context = format!("the signature of {name}");
        let mut vars = HashMap::new();
        self.check_constraints(sig.span, &sig.node.constraints, &mut vars, &context);
        self.check_type(sig.span, &sig.node.ty, &mut vars, &context);
        check_constrained_vars_used(
            sig.span,
            &sig.node.constraints,
            &sig.node.ty,
            &mut self.errors,
        );
    }

    fn check_interface_arity(&mut self, span: Span, name: &str, actual: usize) {
        if let Some(i) = interface(name) {
            if actual != i.vars.len() {
                self.errors.push(TypeError {
                    span,
                    kind: TypeErrorKind::InterfaceArity {
                        interface: name.to_string(),
                        expected: i.vars.len(),
                        actual,
                    },
                });
            }
        }
    }

    fn check_constraints(
        &mut self,
        span: Span,
        constraints: &[CConstraint],
        vars: &mut HashMap<String, Kind>,
        context: &str,
    ) {
        for c in constraints {
            self.check_interface_arity(span, &c.interface, c.type_vars.len());
            for (position, var) in c.type_vars.iter().enumerate() {
                let k = self.var(var, vars);
                self.expect(
                    span,
                    &k,
                    &arrows(parameter_arity(&c.interface, position)),
                    context,
                );
            }
        }
    }
}

/// A constraint on a variable the type never mentions can never be
/// determined by any use (`f :: Eq b => Int -> Int`).
fn check_constrained_vars_used(
    span: Span,
    constraints: &[CConstraint],
    ty: &CType,
    errors: &mut Vec<TypeError>,
) {
    fn mentions(ty: &CType, var: &str) -> bool {
        match ty {
            CType::Var(v) => v == var,
            CType::VarApp(v, args) => v == var || args.iter().any(|a| mentions(a, var)),
            CType::Named(_, args) | CType::Tuple(args) => args.iter().any(|a| mentions(a, var)),
            CType::Fn(a, b) => mentions(a, var) || mentions(b, var),
            CType::Record(fields, ext) => {
                ext.as_deref() == Some(var) || fields.iter().any(|(_, t)| mentions(t, var))
            }
            CType::Unit => false,
        }
    }
    for c in constraints {
        for var in &c.type_vars {
            if !mentions(ty, var) {
                errors.push(TypeError {
                    span,
                    kind: TypeErrorKind::AmbiguousConstraint {
                        interface: c.interface.clone(),
                        ty: var.clone(),
                    },
                });
            }
        }
    }
}

pub fn check_kinds(decls: &[Spanned<CDecl>]) -> Vec<TypeError> {
    check_kinds_with_hosts(decls, &crate::host::HostDeclarations::default())
}
pub(crate) fn check_kinds_with_hosts(
    decls: &[Spanned<CDecl>],
    hosts: &crate::host::HostDeclarations,
) -> Vec<TypeError> {
    check_kinds_with_imports(decls, hosts, &HashMap::new()).0
}

pub(crate) fn check_kinds_with_imports(
    decls: &[Spanned<CDecl>],
    hosts: &crate::host::HostDeclarations,
    imported: &HashMap<Ref, Kind>,
) -> (Vec<TypeError>, HashMap<Ref, Kind>) {
    let mut kinds = Kinds {
        env: imported.clone(),
        ..Kinds::default()
    };
    for n in &hosts.types {
        kinds.env.insert(
            Ref::Imported {
                module: n.module.clone(),
                name: n.name.clone(),
            },
            Kind::Type,
        );
    }
    for t in &prelude().types {
        let r = if t.module.is_empty() {
            Ref::Builtin(t.name.clone())
        } else {
            Ref::Imported {
                module: t.module.clone(),
                name: t.name.clone(),
            }
        };
        kinds.env.insert(r, arrows(t.params.len()));
    }

    // User type declarations: infer parameter kinds together.
    let mut params_of: Vec<(usize, HashMap<String, Kind>)> = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        if let CDecl::TypeDecl(name, params, _, _) = &d.node {
            let vars: HashMap<String, Kind> =
                params.iter().map(|p| (p.clone(), kinds.fresh())).collect();
            let kind = params.iter().rev().fold(Kind::Type, |acc, p| {
                Kind::Arrow(Box::new(vars[p].clone()), Box::new(acc))
            });
            kinds.env.insert(Ref::TopLevel(name.clone()), kind);
            params_of.push((i, vars));
        }
    }
    for (i, mut vars) in params_of {
        let d = &decls[i];
        if let CDecl::TypeDecl(name, _, variants, _) = &d.node {
            let context = format!("type {name}");
            for (_, fields) in variants {
                for f in fields {
                    kinds.check_type(d.span, f, &mut vars, &context);
                }
            }
        }
    }
    kinds.default_all();

    for d in decls {
        match &d.node {
            CDecl::TypeAlias(name, _, body) => {
                let context = format!("type alias {name}");
                kinds.check_type(d.span, body, &mut HashMap::new(), &context);
            }
            CDecl::Fn(f) => {
                if let Some(sig) = &f.signature {
                    kinds.check_signature(&f.name, sig);
                }
            }
            CDecl::Primitive(name, sig) => kinds.check_signature(name, sig),
            CDecl::Interface(iface) => {
                // The interface's own variable has the kind its arity says
                // (`f` in `Functor f` is `Type -> Type`) in every method.
                let context = format!("interface {}", iface.name);
                let class = CConstraint {
                    interface: iface.name.clone(),
                    type_vars: iface.vars.clone(),
                };
                for (_, sig) in &iface.methods {
                    let mut vars = HashMap::new();
                    kinds.check_constraints(
                        sig.span,
                        std::slice::from_ref(&class),
                        &mut vars,
                        &context,
                    );
                    kinds.check_constraints(sig.span, &sig.node.constraints, &mut vars, &context);
                    kinds.check_type(sig.span, &sig.node.ty, &mut vars, &context);
                }
            }
            CDecl::Instance(inst) => {
                check_constrained_vars_used(
                    d.span,
                    &inst.constraints,
                    &CType::Tuple(inst.targets.clone()),
                    &mut kinds.errors,
                );
                let context = format!("instance {}", inst.interface);
                let mut vars = HashMap::new();
                kinds.check_constraints(d.span, &inst.constraints, &mut vars, &context);
                kinds.check_interface_arity(d.span, &inst.interface, inst.targets.len());
                for (position, target) in inst.targets.iter().enumerate() {
                    let k = kinds.infer(d.span, target, &mut vars, &context);
                    kinds.expect(
                        d.span,
                        &k,
                        &arrows(parameter_arity(&inst.interface, position)),
                        &context,
                    );
                }
            }
            CDecl::TypeDecl(..) => {}
        }
        kinds.default_all();
    }
    let exports = kinds
        .env
        .iter()
        .filter(|(r, _)| matches!(r, Ref::TopLevel(_)))
        .map(|(r, k)| (r.clone(), kinds.resolve(k)))
        .collect();
    (kinds.errors, exports)
}
