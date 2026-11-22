use crate::*;
use knot_canonical::ast::CDecl;
use knot_checker::ast::{NodeId, TDoStmt, TExpr, TPattern, TypedExpr, TypedPattern};
use knot_checker::program::{
    BindingId, Definition, DictExpr, DictParamId, EvidenceSite, ResolvedReference, TypedProgram,
};
use knot_syntax::ast::pattern::PatternLiteral;
use std::collections::HashMap;

/// Lower checked modules together. Library code uses the same path as user code.
/// All currently checked expression forms have executable lowering; annotations
/// remain metadata and user-module loading is handled separately.
pub fn lower(modules: &[TypedProgram]) -> Program {
    let mut out = Program::default();
    lower_into(&mut out, modules);
    out
}
/// Append checked modules to an existing library arena without renumbering it.
pub fn lower_into(out: &mut Program, modules: &[TypedProgram]) {
    let mut next = out.next_local;
    for module in modules {
        let mut l = Lower {
            p: module,
            out,
            next: &mut next,
            bindings: HashMap::new(),
            params: HashMap::new(),
        };
        l.module();
    }
    out.next_local = next;
}
pub(crate) struct Lower<'a> {
    pub(crate) p: &'a TypedProgram,
    pub(crate) out: &'a mut Program,
    next: &'a mut usize,
    bindings: HashMap<BindingId, LocalId>,
    params: HashMap<DictParamId, LocalId>,
}
impl Lower<'_> {
    pub(crate) fn fresh(&mut self) -> LocalId {
        let id = LocalId(*self.next);
        *self.next += 1;
        id
    }
    fn binding(&mut self, id: BindingId) -> LocalId {
        if let Some(local) = self.bindings.get(&id) {
            return *local;
        }
        let local = self.fresh();
        self.bindings.insert(id, local);
        self.out
            .local_names
            .insert(local, self.p.bindings[&id].name.clone());
        local
    }
    fn param(&mut self, id: DictParamId) -> LocalId {
        if let Some(local) = self.params.get(&id) {
            return *local;
        }
        let local = self.fresh();
        self.params.insert(id, local);
        self.out
            .local_names
            .insert(local, format!("dictionary {id:?}"));
        local
    }
    fn origin(&self, id: NodeId) -> Origin {
        Origin {
            node: self.p.node_ref(id),
            span: self.p.nodes[&id].span,
            role: Role::Primary,
        }
    }
    pub(crate) fn add(&mut self, o: Origin, k: Kind) -> ExprId {
        self.out.push(o, k)
    }
    pub(crate) fn app(&mut self, o: Origin, f: ExprId, xs: Vec<ExprId>) -> ExprId {
        if xs.is_empty() {
            f
        } else {
            self.add(o, Kind::App(f, xs))
        }
    }
    pub(crate) fn lambdas(&mut self, o: Origin, params: Vec<LocalId>, mut body: ExprId) -> ExprId {
        for p in params.into_iter().rev() {
            body = self.add(o, Kind::Lam(p, body));
        }
        body
    }
    fn definition(&mut self, d: &Definition) -> ExprId {
        let body = self.expr(&d.body);
        let args = self.p.bindings[&d.binding]
            .parameters
            .iter()
            .map(|p| self.param(p.id))
            .collect();
        self.lambdas(self.origin(d.body.id).synthetic(), args, body)
    }
    pub(crate) fn dictionary(&mut self, d: &DictExpr, o: Origin) -> ExprId {
        match d {
            DictExpr::Param(id) => {
                let p = self.param(*id);
                self.add(o, Kind::Local(p))
            }
            DictExpr::Instance {
                interface,
                head,
                args,
            } => {
                let f = self.add(
                    o,
                    Kind::Global(Global::Instance(interface.clone(), head.clone())),
                );
                let xs = args.iter().map(|a| self.dictionary(a, o)).collect();
                self.app(o, f, xs)
            }
            DictExpr::Super { from, interface } => {
                let d = self.dictionary(from, o);
                self.add(o, Kind::Select(d, Field::Super(interface.clone())))
            }
            DictExpr::Structural {
                interface,
                shape,
                args,
            } => {
                let args = args.iter().map(|arg| self.dictionary(arg, o)).collect();
                self.structural(interface, shape, args, o)
            }
        }
    }
    fn reference(&mut self, site: EvidenceSite, o: Origin) -> ExprId {
        let mut args: Vec<_> = self.p.dictionaries[&site]
            .iter()
            .map(|d| self.dictionary(d, o.synthetic()))
            .collect();
        let f = match &self.p.references[&site] {
            ResolvedReference::Binding(id) => {
                if let Some(g) = &self.p.bindings[id].global {
                    self.add(o, Kind::Global(Global::Value(g.clone())))
                } else {
                    let b = self.binding(*id);
                    self.add(o, Kind::Local(b))
                }
            }
            ResolvedReference::Global(g) => {
                if let Some(signature) = self.p.hosts.functions.get(g) {
                    self.add(o, Kind::Host(g.clone(), signature.clone()))
                } else {
                    self.add(o, Kind::Global(Global::Value(g.clone())))
                }
            }
            ResolvedReference::Method {
                interface,
                name,
                dictionary_index,
            } => {
                let dict = args.remove(*dictionary_index);
                self.add(
                    o,
                    Kind::Select(dict, Field::Method(interface.clone(), name.clone())),
                )
            }
        };
        // The lookup is the primary counterpart; dictionary application is synthetic.
        self.app(o.synthetic(), f, args)
    }
    fn pattern(&mut self, p: &TypedPattern) -> Pattern {
        let ids = &self.p.pattern_bindings[&p.id];
        match &p.node {
            TPattern::Wildcard(_) => Pattern::Wildcard,
            TPattern::Var(_) => Pattern::Bind(self.binding(ids[0])),
            TPattern::As(inner, name) => {
                let id = *ids
                    .iter()
                    .find(|id| self.p.bindings[id].name == *name)
                    .unwrap();
                Pattern::As(self.binding(id), Box::new(self.pattern(inner)))
            }
            TPattern::Literal(PatternLiteral::Int(n)) => {
                Pattern::Lit(Literal::Int(wrapping_int(n)))
            }
            TPattern::Literal(PatternLiteral::Str(s)) => Pattern::Lit(Literal::String(s.clone())),
            TPattern::Unit => Pattern::Lit(Literal::Unit),
            TPattern::Ctor(r, ps) => Pattern::Ctor(
                self.p.qualify(r).unwrap(),
                ps.iter().map(|p| self.pattern(p)).collect(),
            ),
            TPattern::Tuple(ps) => Pattern::Tuple(ps.iter().map(|p| self.pattern(p)).collect()),
            TPattern::Cons(h, t) => {
                Pattern::Ctor(prelude(":"), vec![self.pattern(h), self.pattern(t)])
            }
            TPattern::Nil => Pattern::Ctor(prelude("[]"), vec![]),
            TPattern::Record(fields) => Pattern::Record(
                fields
                    .iter()
                    .map(|(name, _)| {
                        let id = *ids
                            .iter()
                            .find(|id| self.p.bindings[id].name == *name)
                            .unwrap();
                        (name.clone(), self.binding(id))
                    })
                    .collect(),
            ),
        }
    }
    fn lambda(&mut self, p: &TypedPattern, body: ExprId, o: Origin) -> ExprId {
        let pattern = self.pattern(p);
        match pattern {
            Pattern::Bind(id) => self.add(o, Kind::Lam(id, body)),
            Pattern::Wildcard => {
                let id = self.fresh();
                self.add(o, Kind::Lam(id, body))
            }
            _ => {
                let id = self.fresh();
                let arg = self.add(self.origin(p.id).synthetic(), Kind::Local(id));
                let case = self.add(self.origin(p.id), Kind::Case(arg, vec![(pattern, body)]));
                self.add(o, Kind::Lam(id, case))
            }
        }
    }
    fn lets(&mut self, bindings: &[(TypedPattern, TypedExpr)], body: ExprId, o: Origin) -> ExprId {
        let mut out = Vec::new();
        for (pat, rhs) in bindings {
            let mut value = self.expr(rhs);
            if matches!(pat.node, TPattern::Var(_)) {
                let id = self.p.pattern_bindings[&pat.id][0];
                let args = self.p.bindings[&id]
                    .parameters
                    .iter()
                    .map(|p| self.param(p.id))
                    .collect();
                value = self.lambdas(self.origin(rhs.id).synthetic(), args, value);
                out.push((self.binding(id), value));
            } else {
                // A single shared match supplies all projections. Demanding any
                // projection performs the whole pattern, once; the let is lazy.
                let pattern = self.pattern(pat);
                let ids = self.p.pattern_bindings[&pat.id].clone();
                let locals: Vec<_> = ids.iter().map(|id| self.binding(*id)).collect();
                let items = locals
                    .iter()
                    .map(|id| self.add(o.synthetic(), Kind::Local(*id)))
                    .collect();
                let tuple = self.add(o.synthetic(), Kind::Tuple(items));
                let matched = self.add(
                    self.origin(pat.id),
                    Kind::Case(value, vec![(pattern, tuple)]),
                );
                let shared = self.fresh();
                out.push((shared, matched));
                for (i, id) in locals.iter().enumerate() {
                    let shared_ref = self.add(o.synthetic(), Kind::Local(shared));
                    let projection =
                        self.add(o.synthetic(), Kind::Select(shared_ref, Field::Index(i)));
                    out.push((*id, projection));
                }
            }
        }
        self.add(o, Kind::Let(out, body))
    }
    fn expr(&mut self, e: &TypedExpr) -> ExprId {
        let o = self.origin(e.id);
        let syn = o.synthetic();
        let site = EvidenceSite::Expr(e.id);
        match &e.node {
            TExpr::IntLit(n) => {
                let dict = self.dictionary(&self.p.dictionaries[&site][0], syn);
                let method = self.add(
                    syn,
                    Kind::Select(dict, Field::Method("Num".into(), "fromInteger".into())),
                );
                let raw = self.add(syn, Kind::Lit(Literal::Integer(n.clone())));
                self.app(o, method, vec![raw])
            }
            TExpr::FloatLit(n) => {
                let dict = self.dictionary(&self.p.dictionaries[&site][0], syn);
                let method = self.add(
                    syn,
                    Kind::Select(
                        dict,
                        Field::Method("Fractional".into(), "fromRational".into()),
                    ),
                );
                let raw = self.add(syn, Kind::Rational(n.clone()));
                self.app(o, method, vec![raw])
            }
            TExpr::StringLit(s) => self.add(o, Kind::Lit(Literal::String(s.clone()))),
            TExpr::Unit => self.add(o, Kind::Lit(Literal::Unit)),
            TExpr::Hole => unreachable!("checked program contains a hole"),
            TExpr::Var(_) | TExpr::Ctor(_) => self.reference(site, o),
            TExpr::App(f, x) => {
                let f = self.expr(f);
                let x = self.expr(x);
                self.app(o, f, vec![x])
            }
            TExpr::BinOp(_, a, b, _) => {
                let f = self.reference(site, o);
                let a = self.expr(a);
                let b = self.expr(b);
                self.app(syn, f, vec![a, b])
            }
            TExpr::Negate(x, _) => {
                let f = self.reference(site, o);
                let x = self.expr(x);
                self.app(syn, f, vec![x])
            }
            TExpr::Lambda(ps, b) => {
                let mut b = self.expr(b);
                for (i, p) in ps.iter().enumerate().rev() {
                    b = self.lambda(p, b, if i == 0 { o } else { syn });
                }
                b
            }
            TExpr::If(c, t, f) => {
                let c = self.expr(c);
                let t = self.expr(t);
                let f = self.expr(f);
                self.add(
                    o,
                    Kind::Case(
                        c,
                        vec![
                            (Pattern::Ctor(prelude("True"), vec![]), t),
                            (Pattern::Ctor(prelude("False"), vec![]), f),
                        ],
                    ),
                )
            }
            TExpr::Let(bs, b) => {
                let b = self.expr(b);
                self.lets(bs, b, o)
            }
            TExpr::Case(x, arms) => {
                let x = self.expr(x);
                let arms = arms
                    .iter()
                    .map(|(p, e)| (self.pattern(p), self.expr(e)))
                    .collect();
                self.add(o, Kind::Case(x, arms))
            }
            TExpr::Do(stmts, b) => {
                let mut body = self.expr(b);
                for (i, s) in stmts.iter().enumerate().rev() {
                    body = match s {
                        TDoStmt::Let(bs) => self.lets(bs, body, syn),
                        TDoStmt::Bind(p, x, _) => {
                            let bind = self.reference(
                                EvidenceSite::DoBind {
                                    block: e.id,
                                    statement: i,
                                },
                                syn,
                            );
                            let x = self.expr(x);
                            let cont = self.lambda(p, body, syn);
                            self.app(syn, bind, vec![x, cont])
                        }
                        TDoStmt::Expr(x, _) => {
                            let bind = self.reference(
                                EvidenceSite::DoBind {
                                    block: e.id,
                                    statement: i,
                                },
                                syn,
                            );
                            let x = self.expr(x);
                            let ignored = self.fresh();
                            let cont = self.add(syn, Kind::Lam(ignored, body));
                            self.app(syn, bind, vec![x, cont])
                        }
                    }
                }
                // A one-element let preserves the do node's primary origin.
                self.add(o, Kind::Let(vec![], body))
            }
            TExpr::List(xs) => {
                let mut tail = self.add(syn, Kind::Ctor(prelude("[]"), 0));
                for x in xs.iter().rev() {
                    let head = self.expr(x);
                    let cons = self.add(syn, Kind::Ctor(prelude(":"), 2));
                    tail = self.app(syn, cons, vec![head, tail]);
                }
                self.add(o, Kind::Let(vec![], tail))
            }
            TExpr::Tuple(xs) => {
                let xs = xs.iter().map(|x| self.expr(x)).collect();
                self.add(o, Kind::Tuple(xs))
            }
            TExpr::Record(fs) => {
                let mut fs: Vec<_> = fs.iter().map(|(n, x)| (n.clone(), self.expr(x))).collect();
                fs.sort_by(|a, b| a.0.cmp(&b.0));
                self.add(o, Kind::Record(fs))
            }
            TExpr::RecordUpdate(x, fs) => {
                let x = self.expr(x);
                let fs = fs.iter().map(|(n, x)| (n.clone(), self.expr(x))).collect();
                self.add(o, Kind::RecordUpdate(x, fs))
            }
            TExpr::FieldAccess(x, n) => {
                let x = self.expr(x);
                self.add(o, Kind::Select(x, Field::Named(n.clone())))
            }
            TExpr::Annotated(_, x) => {
                let x = self.expr(x);
                self.add(o, Kind::Let(vec![], x))
            }
        }
    }
    fn module(&mut self) {
        for d in &self.p.definitions {
            let body = self.definition(d);
            let name = self.p.bindings[&d.binding].global.clone().unwrap();
            self.out.globals.insert(Global::Value(name), body);
        }
        for d in &self.p.defaults {
            let body = self.definition(&d.definition);
            let name = self.p.bindings[&d.definition.binding].name.clone();
            self.out
                .globals
                .insert(Global::Default(d.interface.clone(), name), body);
        }
        for (i, d) in self.p.canonical.decls.iter().enumerate() {
            let o = self.origin(self.p.declaration_nodes[i]).synthetic();
            match &d.node {
                CDecl::Primitive(name, _) => {
                    let k = if name == ":" {
                        Kind::Ctor(prelude(":"), 2)
                    } else {
                        Kind::Prim(name.clone())
                    };
                    let e = self.add(o, k);
                    self.out.globals.insert(
                        Global::Value(GlobalName {
                            module: self.p.canonical.name.clone(),
                            name: name.clone(),
                        }),
                        e,
                    );
                }
                CDecl::TypeDecl(_, _, ctors, _) => {
                    for (name, args) in ctors {
                        let name = GlobalName {
                            module: self.p.canonical.name.clone(),
                            name: name.clone(),
                        };
                        let e = self.add(o, Kind::Ctor(name.clone(), args.len()));
                        self.out.globals.insert(Global::Value(name), e);
                    }
                }
                _ => {}
            }
        }
        for inst in &self.p.instances {
            let o = self
                .origin(self.p.declaration_nodes[inst.declaration])
                .synthetic();
            let mut fields = Vec::new();
            let self_id = self.fresh();
            let interface = knot_canonical::prelude::prelude()
                .interfaces
                .iter()
                .find(|i| i.name == inst.interface)
                .unwrap();
            let methods = &interface.methods;
            for (name, _) in methods {
                let body = if let Some(d) = inst
                    .methods
                    .iter()
                    .find(|d| self.p.bindings[&d.binding].name == *name)
                {
                    self.definition(d)
                } else if inst.derived
                    && matches!(
                        name.as_str(),
                        "==" | "compare" | "showsPrec" | "<>" | "empty"
                    )
                {
                    self.derived_method(inst, name, o)
                } else {
                    let f = self.add(
                        o,
                        Kind::Global(Global::Default(inst.interface.clone(), name.clone())),
                    );
                    let this = self.add(o, Kind::Local(self_id));
                    self.app(o, f, vec![this])
                };
                fields.push((Field::Method(inst.interface.clone(), name.clone()), body));
            }
            for (iface, d) in &inst.superclasses {
                let d = self.dictionary(d, o);
                fields.push((Field::Super(iface.clone()), d));
            }
            let dict = self.add(o, Kind::Dict(fields));
            let this = self.add(o, Kind::Local(self_id));
            let body = self.add(o, Kind::Let(vec![(self_id, dict)], this));
            let args = inst.context.iter().map(|p| self.param(p.id)).collect();
            let body = self.lambdas(o, args, body);
            self.out.globals.insert(
                Global::Instance(inst.interface.clone(), inst.head.clone()),
                body,
            );
        }
    }
}
