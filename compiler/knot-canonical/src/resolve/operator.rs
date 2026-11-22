//! Groups a flat infix chain (`knot_syntax::ast::expr::Expr::OpChain`) into
//! nested `CExpr::BinOp`s by fixity — the algorithm in the Haskell 2010
//! report, §10.6. It runs here, not in the parser, because an operator's
//! fixity is only known once its name is resolved: a module's own
//! `infixl 6 (+++)` can come after the operator's uses, and a shadowing
//! top-level `(+)` has its own fixity rather than the prelude's.
//!
//! Two neighbouring operators at the same precedence group only when both
//! are left- or both right-associative. Anything else (`a == b == c`, or
//! mixing an `infixl 6` with an `infixr 6`) is `CannotMixOperators`; the
//! chain is then grouped to the left so resolution can keep going.

use knot_syntax::ast::decl::{Assoc, Fixity};
use knot_syntax::ast::expr::Expr;
use knot_syntax::ast::Name;
use knot_syntax::span::{Span, Spanned};

use crate::ast::{CExpr, Ref};
use crate::env::Env;
use crate::error::{CanonError, CanonErrorKind};
use crate::resolve::expr::resolve_expr;
use crate::resolve::{unresolved_to_ref, NameKind};

struct Operator {
    name: String,
    reference: Ref,
    fixity: Fixity,
    span: Span,
}

pub fn resolve_chain(
    env: &mut Env,
    first: &Spanned<Expr>,
    rest: &[(Spanned<Name>, Spanned<Expr>)],
    errors: &mut Vec<CanonError>,
) -> Spanned<CExpr> {
    let first = resolve_expr(env, first, errors);
    let mut operators = Vec::new();
    let mut operands = Vec::new();
    for (op, operand) in rest {
        let reference = match env.resolve_value(&op.node) {
            Ok(r) => r,
            Err(kind) => unresolved_to_ref(kind, &op.node, NameKind::Value, op.span, errors),
        };
        operators.push(Operator {
            name: op.node.clone(),
            fixity: env.fixity_of(&reference),
            reference,
            span: op.span,
        });
        operands.push(resolve_expr(env, operand, errors));
    }
    let mut chain = Chain {
        operators,
        operands: operands.into_iter().map(Some).collect(),
        next: 0,
        conflict: None,
    };
    let grouped = chain.group(None, first);
    errors.extend(chain.conflict);
    grouped
}

struct Chain {
    operators: Vec<Operator>,
    /// `operands[i]` is the right operand of `operators[i]`; taken once used.
    operands: Vec<Option<Spanned<CExpr>>>,
    next: usize,
    /// The first grouping conflict, reported once per chain.
    conflict: Option<CanonError>,
}

impl Chain {
    /// Extends `left` with every following operator that binds tighter than
    /// `outer` (the operator `left` is the right operand of, if any).
    fn group(&mut self, outer: Option<usize>, mut left: Spanned<CExpr>) -> Spanned<CExpr> {
        while self.next < self.operators.len() {
            let op = self.next;
            if let Some(outer) = outer {
                let (a, b) = (self.operators[outer].fixity, self.operators[op].fixity);
                if a.precedence == b.precedence && (a.assoc != b.assoc || a.assoc == Assoc::None) {
                    self.conflict.get_or_insert_with(|| {
                        CanonError::new(
                            CanonErrorKind::CannotMixOperators {
                                left: self.operators[outer].name.clone(),
                                right: self.operators[op].name.clone(),
                            },
                            self.operators[op].span,
                        )
                    });
                    return left;
                }
                if a.precedence > b.precedence
                    || (a.precedence == b.precedence && a.assoc == Assoc::Left)
                {
                    return left;
                }
            }
            self.next += 1;
            let operand = self.operands[op].take().expect("each operand is used once");
            let right = self.group(Some(op), operand);
            let span = Span::new(left.span.start, right.span.end);
            left = Spanned::new(
                span,
                CExpr::BinOp(
                    self.operators[op].reference.clone(),
                    Box::new(left),
                    Box::new(right),
                ),
            );
        }
        left
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{CDecl, CExpr, Ref};
    use crate::error::CanonErrorKind;

    fn canon(src: &str) -> Result<Vec<CDecl>, Vec<CanonErrorKind>> {
        let mut state = knot_syntax::ParseState::new(src);
        let decls = state.parse_decls().expect("parses");
        assert!(state.is_eof(), "leftover input in {src:?}");
        crate::canonicalize_decls(&decls)
            .map(|ds| ds.into_iter().map(|d| d.node).collect())
            .map_err(|es| es.into_iter().map(|e| e.kind).collect())
    }

    /// The body of the binding `it`, printed fully parenthesized.
    fn grouped(src: &str) -> String {
        let decls = canon(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
        let body = decls
            .iter()
            .find_map(|d| match d {
                CDecl::Fn(f) if f.name == "it" => Some(&f.body.node),
                _ => None,
            })
            .expect("a binding named `it`");
        show(body)
    }

    fn show(e: &CExpr) -> String {
        match e {
            CExpr::BinOp(op, l, r) => {
                let name = match op {
                    Ref::Builtin(n) | Ref::TopLevel(n) | Ref::Local(n) | Ref::Unresolved(n) => n,
                    Ref::Imported { name, .. } => name,
                };
                format!("({} {name} {})", show(&l.node), show(&r.node))
            }
            CExpr::Var(Ref::Local(n) | Ref::TopLevel(n) | Ref::Builtin(n)) => n.clone(),
            CExpr::IntLit(n) => n.to_string(),
            CExpr::List(items) if items.is_empty() => "[]".to_string(),
            CExpr::App(f, a) => format!("{} {}", show(&f.node), show(&a.node)),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn prelude_precedences() {
        assert_eq!(grouped("it = 1 + 2 * 3"), "(1 + (2 * 3))");
        assert_eq!(grouped("it = 1 * 2 + 3"), "((1 * 2) + 3)");
        assert_eq!(grouped("it a b c = a && b || c"), "((a && b) || c)");
        assert_eq!(grouped("it m f = m <*> f >>= f"), "((m <*> f) >>= f)");
        assert_eq!(grouped("it a b = a + 1 == b"), "((a + 1) == b)");
    }

    #[test]
    fn prelude_associativity() {
        assert_eq!(grouped("it = 2 ^ 3 ^ 2"), "(2 ^ (3 ^ 2))");
        assert_eq!(grouped("it = 1 : 2 : []"), "(1 : (2 : []))");
        assert_eq!(grouped("it = 1 - 2 - 3"), "((1 - 2) - 3)");
        assert_eq!(grouped("it x f g = x |> f |> g"), "((x |> f) |> g)");
    }

    #[test]
    fn backticked_functions_use_their_fixity() {
        assert_eq!(grouped("it = 7 `div` 2 + 1"), "((7 div 2) + 1)");
        assert_eq!(grouped("it = 1 + 7 `mod` 2"), "(1 + (7 mod 2))");
    }

    #[test]
    fn an_operator_without_a_fixity_is_infixl_9() {
        let src = "(+++) a b = a\nit = 1 + 2 +++ 3 +++ 4";
        assert_eq!(grouped(src), "(1 + ((2 +++ 3) +++ 4))");
        let src = "plus a b = a\nit = 1 * 2 `plus` 3";
        assert_eq!(grouped(src), "(1 * (2 plus 3))");
    }

    #[test]
    fn a_fixity_declaration_applies_wherever_it_is_written() {
        let src = "it = 1 +++ 2 * 3 +++ 4\n(+++) a b = a\ninfixr 5 (+++)";
        assert_eq!(grouped(src), "(1 +++ ((2 * 3) +++ 4))");
    }

    #[test]
    fn a_shadowing_operator_has_its_own_fixity() {
        let src = "(+) a b = a\nit = 1 + 2 * 3";
        assert_eq!(grouped(src), "((1 + 2) * 3)");
    }

    #[test]
    fn non_associative_operators_do_not_chain() {
        let errors = canon("it a b c = a == b == c").unwrap_err();
        assert_eq!(
            errors,
            vec![CanonErrorKind::CannotMixOperators {
                left: "==".to_string(),
                right: "==".to_string()
            }]
        );
        assert!(canon("it a b c = a == b < c").is_err());
        assert!(canon("it a b c d = a == b == c == d").unwrap_err().len() == 1);
    }

    #[test]
    fn left_and_right_associative_operators_do_not_mix() {
        let src = "(+++) a b = a\ninfixr 6 (+++)\nit = 1 + 2 +++ 3";
        assert!(matches!(
            canon(src).unwrap_err().as_slice(),
            [CanonErrorKind::CannotMixOperators { .. }]
        ));
    }

    #[test]
    fn an_unknown_operator_is_unbound() {
        assert_eq!(
            canon("it = 1 <+> 2").unwrap_err(),
            vec![CanonErrorKind::UnboundVariable("<+>".to_string())]
        );
    }
}
