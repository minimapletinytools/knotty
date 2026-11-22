//! A folded expression still refers to bindings outside it. Match their checked
//! identities to ports in the enclosing graph, even across an inner shadow.
use super::Projection;
use knot_checker::{
    ast::{TDoStmt, TExpr, TypedExpr},
    program::{EvidenceSite, ResolvedReference},
};

impl Projection<'_> {
    pub(super) fn fallback_ports(&self, expr: &TypedExpr) -> Vec<usize> {
        let mut ports = Vec::new();
        visit(expr, &mut |node| {
            if let Some(ResolvedReference::Binding(id)) =
                self.program.references.get(&EvidenceSite::Expr(node.id))
            {
                if let Some((_, port)) = self.active.iter().find(|(binding, _)| binding == id) {
                    if !ports.contains(port) {
                        ports.push(*port);
                    }
                }
            }
        });
        ports
    }
}

fn visit(expr: &TypedExpr, f: &mut impl FnMut(&TypedExpr)) {
    f(expr);
    match &expr.node {
        TExpr::Negate(e, _)
        | TExpr::Lambda(_, e)
        | TExpr::FieldAccess(e, _)
        | TExpr::Annotated(_, e) => visit(e, f),
        TExpr::App(a, b) | TExpr::BinOp(_, a, b, _) => {
            visit(a, f);
            visit(b, f);
        }
        TExpr::If(test, yes, no) => {
            visit(test, f);
            visit(yes, f);
            visit(no, f);
        }
        TExpr::List(items) | TExpr::Tuple(items) => {
            for value in items {
                visit(value, f);
            }
        }
        TExpr::Record(fields) | TExpr::RecordUpdate(_, fields) => {
            if let TExpr::RecordUpdate(base, _) = &expr.node {
                visit(base, f);
            }
            for (_, value) in fields {
                visit(value, f);
            }
        }
        TExpr::Case(subject, arms) => {
            visit(subject, f);
            for (_, body) in arms {
                visit(body, f);
            }
        }
        TExpr::Let(bindings, body) => {
            for (_, rhs) in bindings {
                visit(rhs, f);
            }
            visit(body, f);
        }
        TExpr::Do(steps, body) => {
            for step in steps {
                match step {
                    TDoStmt::Bind(_, rhs, _) | TDoStmt::Expr(rhs, _) => visit(rhs, f),
                    TDoStmt::Let(bindings) => {
                        for (_, rhs) in bindings {
                            visit(rhs, f);
                        }
                    }
                }
            }
            visit(body, f);
        }
        TExpr::IntLit(_)
        | TExpr::FloatLit(_)
        | TExpr::StringLit(_)
        | TExpr::Unit
        | TExpr::Hole
        | TExpr::Var(_)
        | TExpr::Ctor(_) => {}
    }
}
