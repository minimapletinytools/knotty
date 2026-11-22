//! Post-parse structural checks that don't need type information: fixity
//! declarations and duplicate top-level bindings (which enforce the absence
//! of multi-clause function definitions).

use crate::ast::decl::{Decl, Module};
use crate::span::Span;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub message: String,
    pub span: Span,
}

pub fn validate_module(module: &Module) -> Vec<ValidationError> {
    let mut errors = validate_library_module(module);
    check_library_only_decls(module, &mut errors);
    errors
}

/// Everything `validate_module` checks except the library-only rule: the
/// prelude may declare interfaces, primitives, and opaque types.
pub fn validate_library_module(module: &Module) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    check_duplicate_fn_names(module, &mut errors);
    check_fixity_decls(module, &mut errors);
    errors
}

fn check_duplicate_fn_names(module: &Module, errors: &mut Vec<ValidationError>) {
    let mut seen: HashMap<&str, Span> = HashMap::new();
    for decl in &module.decls {
        if let Decl::Fn(f) = &decl.node {
            if seen.contains_key(f.name.as_str()) {
                errors.push(ValidationError {
                    message: format!(
                        "`{}` is defined more than once -- Knot doesn't support multi-clause \
                         function definitions; use `case` instead",
                        f.name
                    ),
                    span: decl.span,
                });
            } else {
                seen.insert(&f.name, decl.span);
            }
        }
    }
}

/// Interfaces, primitives (signatures with no definition), and opaque types
/// (`type Int`) exist only in library modules: the interface set is closed
/// (spec §2.3), and a primitive needs the runtime to provide it.
fn check_library_only_decls(module: &Module, errors: &mut Vec<ValidationError>) {
    for decl in &module.decls {
        let message = match &decl.node {
            Decl::Interface(i) => format!(
                "`interface {}`: user modules can't declare interfaces; the set is fixed by the prelude",
                i.name
            ),
            Decl::Primitive(name, _) => {
                format!("`{name}` has a type signature but no definition")
            }
            Decl::TypeDecl(name, _, variants, _) if variants.is_empty() => {
                format!("`type {name}` needs at least one constructor")
            }
            _ => continue,
        };
        errors.push(ValidationError {
            message,
            span: decl.span,
        });
    }
}

/// A fixity declaration must name a function or operator defined at this
/// module's top level (like Haskell's, it travels with the definition), and
/// an operator gets at most one.
fn check_fixity_decls(module: &Module, errors: &mut Vec<ValidationError>) {
    let defined: std::collections::HashSet<&str> = module
        .decls
        .iter()
        .flat_map(|d| match &d.node {
            Decl::Fn(f) => vec![f.name.as_str()],
            Decl::Primitive(name, _) => vec![name.as_str()],
            Decl::Interface(i) => i.methods.iter().map(|(m, _)| m.as_str()).collect(),
            _ => Vec::new(),
        })
        .collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for decl in &module.decls {
        let Decl::Fixity(fixity) = &decl.node else {
            continue;
        };
        for op in &fixity.operators {
            if !defined.contains(op.node.as_str()) {
                errors.push(ValidationError {
                    message: format!(
                        "fixity declaration for `{}`, which this module doesn't define",
                        op.node
                    ),
                    span: op.span,
                });
            }
            if !seen.insert(op.node.as_str()) {
                errors.push(ValidationError {
                    message: format!("`{}` has more than one fixity declaration", op.node),
                    span: op.span,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_src(src: &str) -> Vec<ValidationError> {
        let module_src = format!("module Test exposing (..)\n\n{src}");
        let module = crate::ParseState::new(&module_src)
            .parse_module()
            .expect("fixture failed to parse");
        validate_module(&module)
    }

    #[test]
    fn no_errors_for_well_formed_module() {
        assert!(validate_src("x = (1, 2, 3)").is_empty());
    }

    #[test]
    fn duplicate_top_level_binding_is_rejected() {
        let errors = validate_src("f 0 = 1\nf n = n");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("defined more than once"));
    }

    #[test]
    fn larger_tuples_are_valid_in_types_values_and_patterns() {
        assert!(validate_src(
            "type alias Quad a = (a, a, a, a)\nquad :: Quad Int\nquad = (1, 2, 3, 4)\nf (a, b, c, d) = [(a, b, c, d, (a, b, c, d))]"
        ).is_empty());
    }

    #[test]
    fn fixity_for_a_defined_operator_is_fine() {
        let src = "infixl 6 (+++)\n(+++) a b = a\ninfixl 7 `times`\ntimes a b = a";
        assert!(validate_src(src).is_empty());
    }

    #[test]
    fn fixity_without_a_definition_is_rejected() {
        // Including a prelude operator: its fixity is the prelude's.
        assert_eq!(validate_src("infixl 6 (+++)").len(), 1);
        assert_eq!(validate_src("infixr 6 (+)").len(), 1);
    }

    #[test]
    fn two_fixities_for_one_operator_are_rejected() {
        let src = "infixl 6 (+++)\ninfixr 6 (+++)\n(+++) a b = a";
        assert_eq!(validate_src(src).len(), 1);
    }
}
