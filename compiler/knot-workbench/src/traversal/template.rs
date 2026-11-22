//! Parameterize expression scalars only. Patterns, annotations, types, names,
//! constructors and operators remain part of the checked template identity.
//! This key is for typechecking metadata only, never executable value reuse.
use knot_syntax::{
    ast::{
        decl::{Decl, Module},
        expr::{DoStmt, Expr},
    },
    Span, Spanned,
};

pub(super) struct Template {
    pub source: String,
}
impl Template {
    pub fn new(source: &str, raw: &Module) -> Self {
        let mut scalars = Vec::new();
        let mut shape = String::new();
        for decl in &raw.decls {
            // Generated probes contain ordinary definitions. Other declarations
            // are retained verbatim and do not participate in parameterization.
            if let Decl::Fn(f) = &decl.node {
                shape.push_str(&format!("fn{}:", f.params.len()));
                collect(&f.body, &mut scalars, &mut shape);
            }
        }
        scalars.sort_by_key(|(span, _)| span.start);
        let mut normalized = String::with_capacity(source.len());
        let mut cursor = 0;
        for (span, value) in scalars {
            normalized.push_str(&source[cursor..span.start as usize]);
            normalized.push_str(value);
            cursor = span.end as usize;
        }
        normalized.push_str(&source[cursor..]);
        Self {
            source: format!("{}:{normalized}{shape}", normalized.len()),
        }
    }
}
fn collect(e: &Spanned<Expr>, out: &mut Vec<(Span, &'static str)>, shape: &mut String) {
    // Layout is parsed before normalization. Retain the expression tree shape
    // in the cache key so two layouts cannot collide when literals shrink.
    shape.push(match &e.node {
        Expr::IntLit(_) => 'i',
        Expr::FloatLit(_) => 'f',
        Expr::StringLit(_) => 's',
        Expr::Unit => 'u',
        Expr::Var(_) => 'v',
        Expr::Ctor(_) => 'c',
        Expr::Hole => 'h',
        Expr::Lambda(..) => 'l',
        Expr::App(..) => 'a',
        Expr::OpChain(..) => 'o',
        Expr::OpRef(_) => 'r',
        Expr::Negate(_) => 'n',
        Expr::If(..) => 'b',
        Expr::Let(..) => 'e',
        Expr::Case(..) => 'k',
        Expr::Do(..) => 'd',
        Expr::List(_) => 'j',
        Expr::Tuple(_) => 't',
        Expr::Record(_) => 'q',
        Expr::RecordUpdate(..) => 'p',
        Expr::FieldAccess(..) => 'x',
        Expr::Annotated(..) => 'z',
    });
    match &e.node {
        Expr::IntLit(_) => out.push((e.span, "0")),
        Expr::FloatLit(_) => out.push((e.span, "0.0")),
        Expr::StringLit(_) => out.push((e.span, "\"\"")),
        Expr::App(a, b) => {
            collect(a, out, shape);
            collect(b, out, shape);
        }
        Expr::OpChain(a, rest) => {
            collect(a, out, shape);
            for (_, b) in rest {
                collect(b, out, shape);
            }
        }
        Expr::If(a, b, c) => {
            collect(a, out, shape);
            collect(b, out, shape);
            collect(c, out, shape);
        }
        Expr::Lambda(ps, b) => {
            shape.push_str(&ps.len().to_string());
            collect(b, out, shape);
        }
        Expr::Negate(b) | Expr::Annotated(_, b) | Expr::FieldAccess(b, _) => collect(b, out, shape),
        Expr::Let(bindings, b) => {
            for (_, value) in bindings {
                collect(value, out, shape);
            }
            collect(b, out, shape);
        }
        Expr::Case(value, arms) => {
            collect(value, out, shape);
            for (_, b) in arms {
                collect(b, out, shape);
            }
        }
        Expr::List(xs) | Expr::Tuple(xs) => {
            for x in xs {
                collect(x, out, shape);
            }
        }
        Expr::Record(fs) => {
            for (_, v) in fs {
                collect(v, out, shape);
            }
        }
        Expr::RecordUpdate(b, fs) => {
            collect(b, out, shape);
            for (_, v) in fs {
                collect(v, out, shape);
            }
        }
        Expr::Do(statements, b) => {
            for stmt in statements {
                match stmt {
                    DoStmt::Bind(_, v) | DoStmt::Expr(v) => collect(v, out, shape),
                    DoStmt::Let(bindings) => {
                        for (_, v) in bindings {
                            collect(v, out, shape);
                        }
                    }
                }
            }
            collect(b, out, shape);
        }
        _ => {}
    }
    shape.push(')');
}
