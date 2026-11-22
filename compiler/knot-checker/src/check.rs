//! The checker's entry points: type-check one already-canonicalized
//! module against the prelude.
//!
//! `check_module_report` is the diagnostic answer — errors, exhaustiveness
//! warnings, and every top-level binding's type as Knot text (what the
//! corpus's `-- type:` golden lines are compared against).
//! `check_module`/`check_module_with_warnings` are narrower views of it.
//! `check_program`/`check_library_program` additionally export owned typed
//! bodies, binding identities and dictionary expressions for lowering.

use std::{collections::HashMap, sync::Arc};

use crate::interface::instance::InstanceTable;
use crate::modules::CheckedInterface;
use knot_canonical::ast::{CDecl, Ref};
use knot_syntax::span::Spanned;

use crate::constrain::decl::{constrain_program, seed_user_constructors, ConstrainedModule};
use crate::error::TypeError;
use crate::interface::instance::{build_instance_table, check_instance_methods};
use crate::solve::{solve_module, SchemeKey};

#[derive(Debug)]
pub struct CheckReport {
    pub errors: Vec<TypeError>,
    pub warnings: Vec<crate::exhaustiveness::Warning>,
    /// Every top-level binding, in source order, with its type scheme
    /// printed as Knot source (`Eq a => a -> a -> Bool`).
    pub types: Vec<(String, String)>,
}

pub fn check_module(decls: &[Spanned<CDecl>]) -> Vec<TypeError> {
    check_module_report(decls).errors
}

pub fn check_module_with_warnings(
    decls: &[Spanned<CDecl>],
) -> (Vec<TypeError>, Vec<crate::exhaustiveness::Warning>) {
    let report = check_module_report(decls);
    (report.errors, report.warnings)
}

pub fn check_module_report(decls: &[Spanned<CDecl>]) -> CheckReport {
    check_decls(decls, false)
}

/// Check and export a single user module, retaining bodies, schemes and
/// dictionary expressions for lowering. No program is returned on failure.
pub fn check_program(
    module: &knot_canonical::ast::CModule,
) -> Result<crate::program::TypedProgram, CheckReport> {
    crate::program::export(module, check_decls_state(&module.decls, false))
}

/// Check with trusted host declarations. Use the same declaration set as the
/// canonical ModuleRegistry and the runtime registry; imported signatures stay
/// in the typed result so core can retain its host-link contract.
pub fn check_program_with_hosts(
    module: &knot_canonical::ast::CModule,
    hosts: &crate::host::HostDeclarations,
) -> Result<crate::program::TypedProgram, CheckReport> {
    crate::program::export(
        module,
        check_decls_state_with_hosts(&module.decls, false, hosts),
    )
}

/// The same owned output for a library module, including instance methods and
/// interface defaults. It uses the library canonicalization/trust boundary.
pub fn check_library_program(name: &str) -> Result<crate::program::TypedProgram, CheckReport> {
    let module = knot_canonical::canonicalize_library(name)
        .unwrap_or_else(|errs| panic!("{name}.knot doesn't canonicalize: {errs:?}"));
    let checked = check_decls_state(&module.decls, name == "Prelude");
    crate::program::export(&module, checked)
}

pub(crate) struct CheckedModule {
    pub kinds: std::collections::HashMap<knot_canonical::ast::Ref, crate::kind::Kind>,
    pub is_prelude: bool,
    pub hosts: crate::host::HostDeclarations,
    pub sub: crate::var::Substitution,
    pub env: crate::solve::SchemeEnv,
    pub table: Arc<InstanceTable>,
    pub output: ConstrainedModule,
    pub solved: crate::solve::SolveResult,
    pub report: CheckReport,
}

/// Type-checks a library module's bodies (`knot_canonical::prelude::LIBRARY`):
/// every definition, instance method, and interface default against the
/// signatures every other module is checked against.
pub fn check_library(name: &str) -> CheckReport {
    let module = knot_canonical::canonicalize_library(name)
        .unwrap_or_else(|errs| panic!("{name}.knot doesn't canonicalize: {errs:?}"));
    let checked = check_decls_state(&module.decls, name == "Prelude");
    diagnostic_report(&module, checked)
}

/// `is_prelude`: the Prelude's own instances (declared and derived) are the
/// builtin instance table already, so they aren't registered a second time.
fn check_decls(decls: &[Spanned<CDecl>], is_prelude: bool) -> CheckReport {
    let checked = check_decls_state(decls, is_prelude);
    let module = knot_canonical::ast::CModule {
        name: vec!["Main".into()],
        exposing: knot_syntax::ast::decl::Exposing::All,
        imports: Vec::new(),
        decls: decls.to_vec(),
    };
    diagnostic_report(&module, checked)
}

fn diagnostic_report(module: &knot_canonical::ast::CModule, checked: CheckedModule) -> CheckReport {
    if !checked.report.errors.is_empty() {
        return checked.report;
    }
    let types = checked.report.types.clone();
    // The diagnostic and executable entry points enforce the same dictionary
    // scope invariant. Inference alone can otherwise lose a constraint in a
    // recursive group whose members quantify different variables.
    match crate::program::export(module, checked) {
        Ok(program) => CheckReport {
            errors: Vec::new(),
            warnings: program.warnings,
            types,
        },
        Err(report) => report,
    }
}

fn check_decls_state(decls: &[Spanned<CDecl>], is_prelude: bool) -> CheckedModule {
    check_decls_state_with_hosts(decls, is_prelude, &crate::host::HostDeclarations::default())
}
fn check_decls_state_with_hosts(
    decls: &[Spanned<CDecl>],
    is_prelude: bool,
    hosts: &crate::host::HostDeclarations,
) -> CheckedModule {
    check_decls_with_imports(decls, is_prelude, hosts, &[])
}

/// Check against owned dependency interfaces, freshening all imported schemes.
/// Supply each transitive dependency once; canonicalization must use these same interfaces.
pub fn check_program_with_imports(
    module: &knot_canonical::ast::CModule,
    hosts: &crate::host::HostDeclarations,
    imports: &[&crate::modules::CheckedInterface],
) -> Result<crate::program::TypedProgram, CheckReport> {
    crate::program::export(
        module,
        check_decls_with_imports(&module.decls, false, hosts, imports),
    )
}

fn check_decls_with_imports(
    decls: &[Spanned<CDecl>],
    is_prelude: bool,
    hosts: &crate::host::HostDeclarations,
    imports: &[&crate::modules::CheckedInterface],
) -> CheckedModule {
    let scope = ImportScope::new(imports);
    check_in_scope(decls, is_prelude, hosts, imports, &scope)
}

/// Immutable import evidence for a batch of modules checked against the same
/// interfaces. Each module still gets a fresh inference arena and value schemes.
pub struct PreparedImports {
    interfaces: Vec<Arc<CheckedInterface>>,
    scope: ImportScope,
}

impl PreparedImports {
    pub fn new(interfaces: Vec<Arc<CheckedInterface>>) -> Self {
        let scope = ImportScope::new(&interfaces.iter().map(Arc::as_ref).collect::<Vec<_>>());
        Self { interfaces, scope }
    }

    pub fn interfaces(&self) -> &[Arc<CheckedInterface>] {
        &self.interfaces
    }

    pub fn check(
        &self,
        module: &knot_canonical::ast::CModule,
        hosts: &crate::host::HostDeclarations,
    ) -> Result<crate::program::TypedProgram, CheckReport> {
        let imports = self.interfaces.iter().map(Arc::as_ref).collect::<Vec<_>>();
        crate::program::export(
            module,
            check_in_scope(&module.decls, false, hosts, &imports, &self.scope),
        )
    }
}

struct ImportScope {
    instances: Arc<InstanceTable>,
    kinds: HashMap<Ref, crate::kind::Kind>,
    errors: Vec<TypeError>,
}

impl ImportScope {
    fn new(imports: &[&CheckedInterface]) -> Self {
        let mut instances = crate::prelude::initial_state().2.clone();
        let mut kinds = HashMap::new();
        let mut errors = Vec::new();
        for interface in imports {
            kinds.extend(interface.kinds.clone());
            errors.extend(instances.import_entries(&interface.instances));
        }
        Self {
            instances: Arc::new(instances),
            kinds,
            errors,
        }
    }
}

fn check_in_scope(
    decls: &[Spanned<CDecl>],
    is_prelude: bool,
    hosts: &crate::host::HostDeclarations,
    imports: &[&CheckedInterface],
    scope: &ImportScope,
) -> CheckedModule {
    let (prelude_sub, prelude_env, _) = crate::prelude::initial_state();
    let (mut sub, mut env) = (prelude_sub.clone(), prelude_env.clone());
    hosts.seed(&mut sub, &mut env);
    seed_user_constructors(&mut sub, &mut env, decls);

    let (mut errors, kinds) = crate::kind::check_kinds_with_imports(decls, hosts, &scope.kinds);
    errors.extend_from_slice(&scope.errors);
    let table = if is_prelude || decls.iter().all(|d| matches!(&d.node, CDecl::Fn(_))) {
        scope.instances.clone()
    } else {
        let (mut table, instance_errors) = build_instance_table(decls, &scope.instances);
        errors.extend(instance_errors);
        table.merge_from((*scope.instances).clone());
        Arc::new(table)
    };
    errors.extend(check_instance_methods(decls));

    let output = constrain_program(&mut sub, decls);
    // Canonicalization has already resolved names against complete interfaces.
    // Only value lookups need copied schemes; keep all kinds and instances above
    // so annotations, derived evidence and coherence retain their full context.
    if !imports.is_empty() {
        let used = output.constraint.imported_lookups();
        for interface in imports {
            if let Some(names) = used.get(&interface.name) {
                interface.seed(&mut sub, &mut env, names);
            }
        }
    }
    let mut solved = solve_module(&mut sub, &mut env, &table, &output.constraint);
    errors.append(&mut solved.errors);

    let types = decls
        .iter()
        .filter_map(|d| match &d.node {
            CDecl::Fn(f) => env
                .get(&SchemeKey::TopLevel(f.name.clone()))
                .cloned()
                .map(|scheme| {
                    (
                        f.name.clone(),
                        crate::print::print_scheme(&mut sub, &scheme),
                    )
                }),
            _ => None,
        })
        .collect();

    let report = CheckReport {
        errors,
        warnings: crate::exhaustiveness::check_module_with_imports(decls, imports),
        types,
    };
    CheckedModule {
        kinds,
        hosts: hosts.clone(),
        is_prelude,
        sub,
        env,
        table,
        output,
        solved,
        report,
    }
}

/// Parses, canonicalizes, and checks `src` — a convenience for tests.
/// Panics if `src` doesn't parse or canonicalize.
pub fn check_source(src: &str) -> CheckReport {
    let mut state = knot_syntax::ParseState::new(src);
    let raw = state
        .parse_decls()
        .unwrap_or_else(|e| panic!("parse error {e:?} in {src}"));
    assert!(state.is_eof(), "leftover input: {src}");
    let decls = knot_canonical::canonicalize_decls(&raw)
        .unwrap_or_else(|errs| panic!("canonicalization failed {errs:?} in {src}"));
    check_module_report(&decls)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(src: &str) -> Vec<Spanned<CDecl>> {
        let mut state = knot_syntax::ParseState::new(src);
        let raw = state.parse_decls().unwrap();
        assert!(state.is_eof(), "leftover input: {src}");
        knot_canonical::canonicalize_decls(&raw).unwrap_or_else(|errs| panic!("{errs:?}"))
    }

    #[test]
    fn prepared_helpers_share_imports_and_keep_local_instances_private() {
        let imports = PreparedImports::new(vec![]);
        let check = |source| {
            let module = knot_canonical::ast::CModule {
                name: vec!["Example".into()],
                exposing: knot_syntax::ast::decl::Exposing::All,
                imports: vec![],
                decls: decls(source),
            };
            imports.check(&module, &Default::default())
        };
        let a = check("sameInt :: Int -> Bool\nsameInt x = x == x\n").unwrap();
        let b = check("sameBool :: Bool -> Bool\nsameBool x = x == x\n").unwrap();
        assert!(Arc::ptr_eq(&a.instances_table, &b.instances_table));
        let own = check("type Token = Token deriving (Eq)\ncompare = Token == Token\n").unwrap();
        assert!(!Arc::ptr_eq(&a.instances_table, &own.instances_table));
        assert!(check("type Token = Token\ncompare = Token == Token\n").is_err());
    }

    #[test]
    fn the_library_modules_type_check() {
        for (name, _) in knot_canonical::prelude::LIBRARY {
            let report = check_library(name);
            assert!(
                report.errors.is_empty(),
                "{name}.knot:\n{}",
                report
                    .errors
                    .iter()
                    .map(|e| format!("{e:?}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }

    #[test]
    fn a_plain_builtin_arithmetic_function_type_checks_with_no_hand_wiring() {
        // The exact case that spuriously failed while drafting the
        // corpus-semantic plan, purely from a scratch test forgetting to
        // merge the builtin instance table in by hand.
        let cs = decls("addX :: Float -> Float -> Float\naddX a b = a + b\n");
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_bare_operator_reference_applied_like_a_function_type_checks_the_same_as_infix() {
        // `(+)` doesn't desugar to a lambda (spec §7.6) -- it's a plain
        // `Var` reference, and `(+) a b` is ordinary application. Confirms that
        // second representation type-checks identically to the infix one
        // right above, through the real end-to-end pipeline.
        let cs = decls("addX :: Float -> Float -> Float\naddX a b = (+) a b\n");
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn redeclaring_a_builtin_instance_through_the_real_entry_point_is_a_duplicate_error() {
        // Task #40, end to end through check_module itself (not just
        // build_instance_table directly) -- confirms the fix reaches all
        // the way through the real seeded-prelude-table wiring.
        let cs = decls("instance Eq Int where\n  (==) a b = False\n");
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::DuplicateInstance { interface } if interface == "Eq"
        )));
    }

    #[test]
    fn a_user_defined_adt_with_a_declared_instance_type_checks_end_to_end() {
        let cs = decls(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n\
             compareShapes :: Shape -> Shape -> Bool\n\
             compareShapes a b = a == b\n",
        );
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_genuinely_missing_instance_is_still_reported() {
        let cs = decls(
            "type Shape = Circle Float\n\
             compareShapes :: Shape -> Shape -> Bool\n\
             compareShapes a b = a == b\n",
        );
        let errors = check_module(&cs);
        assert!(errors
            .iter()
            .any(|e| matches!(&e.kind, crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Eq")));
    }

    #[test]
    fn a_user_instance_method_bodys_own_obligations_are_checked_too() {
        // An obligation that only appears inside an instance method's body must
        // still be checked.
        let cs = decls(concat!(
            "type alias Point = { x : Float }\n",
            "instance Semigroup Point where\n",
            "  (<>) a b = { x = a.x + b.x }\n",
        ));
        let errors = check_module(&cs);
        // Semigroup on a Record target is the InstanceTargetNotNominal
        // gap fixed alongside this entry point -- but the method body's
        // own `a.x + b.x` should still type-check against `Num Float`
        // cleanly regardless, proving check_pending reaches instance
        // method bodies at all.
        assert!(!errors
            .iter()
            .any(|e| matches!(&e.kind, crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Num")));
    }

    #[test]
    fn a_functor_and_foldable_instances_methods_are_checked_through_the_real_entry_point() {
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b = case b of\n",
            "    Box x -> Box (f x)\n",
            "instance Foldable Box where\n",
            "  foldl f z b = case b of\n",
            "    Box x -> f z x\n",
            "  foldr f z b = case b of\n",
            "    Box x -> f x z\n",
            "  length b = 1\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_user_signature_can_be_generic_over_any_foldable() {
        // The feature this session's own VarApp grammar/canonical/checker
        // work adds (spec §10.5): a user's own signed function, not just
        // the hand-built prelude schemes, can now be genuinely polymorphic
        // over "any Foldable" -- one shared signature, called here
        // against both a user-declared custom instance and the builtin
        // List.
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b = case b of\n",
            "    Box x -> Box (f x)\n",
            "instance Foldable Box where\n",
            "  foldl f z b = case b of\n",
            "    Box x -> f z x\n",
            "  foldr f z b = case b of\n",
            "    Box x -> f x z\n",
            "  length b = 1\n",
            "countIt :: Foldable f => f Int -> Int\n",
            "countIt xs = length xs\n",
            "countedBox :: Int\n",
            "countedBox = countIt (Box 5)\n",
            "countedList :: Int\n",
            "countedList = countIt [1, 2, 3]\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_user_signature_generic_over_foldable_rejects_a_type_with_no_foldable_instance() {
        // `Box` has no declared `Foldable` instance at all -- proves the
        // constraint is genuinely enforced, not vacuously accepted just
        // because the signature parses.
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "countIt :: Foldable f => f Int -> Int\n",
            "countIt xs = length xs\n",
            "bad :: Int\n",
            "bad = countIt (Box 5)\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Foldable"
        )));
    }

    #[test]
    fn a_user_signature_can_be_generic_over_any_monad() {
        // Same feature, the other constructor-shaped interface family -- a
        // signed function using `(>>=)`/`pure` through a rigid
        // Monad-constrained `f` (Monad implies Applicative, §10.3, so
        // `pure` is covered by the same `given` fact), called against a
        // user-declared custom instance, a builtin Maybe, and a builtin
        // Result.
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b = case b of\n",
            "    Box x -> Box (f x)\n",
            "instance Applicative Box where\n",
            "  pure x = Box x\n",
            "  (<*>) bf bx = case (bf, bx) of\n",
            "    (Box f, Box x) -> Box (f x)\n",
            "instance Monad Box where\n",
            "  (>>=) b f = case b of\n",
            "    Box x -> f x\n",
            "passThrough :: Monad f => f Int -> f Int\n",
            "passThrough fa = fa >>= (\\x -> pure x)\n",
            "throughBox :: Box Int\n",
            "throughBox = passThrough (Box 5)\n",
            "throughMaybe :: Maybe Int\n",
            "throughMaybe = passThrough (Just 5)\n",
            "throughResult :: Result String Int\n",
            "throughResult = passThrough (Ok 5)\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_traversable_instances_traverse_type_checks_against_a_real_applicative() {
        // `traverse`'s own `f` (spec §10.5) is a second constructor
        // variable, distinct from `Self`/`t` (`Box` here) -- this proves
        // both that the method body type-checks against its own `f`
        // (`map Box (f x)` needs `Functor f`, granted transitively from
        // the method's own `Applicative f` given fact via the ordinary
        // Applicative-implies-Functor superclass closure, §10.3) and that
        // a real call site correctly infers `f` all the way to a concrete
        // `Maybe`.
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b = case b of\n",
            "    Box x -> Box (f x)\n",
            "instance Foldable Box where\n",
            "  foldl f z b = case b of\n",
            "    Box x -> f z x\n",
            "  foldr f z b = case b of\n",
            "    Box x -> f x z\n",
            "  length b = 1\n",
            "instance Traversable Box where\n",
            "  traverse f b = case b of\n",
            "    Box x -> map Box (f x)\n",
            "useTraverse :: Box Int -> Maybe (Box Int)\n",
            "useTraverse b = traverse (\\x -> Just x) b\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn traverse_rejects_a_type_with_no_traversable_instance() {
        // `Box` has no declared `Traversable` instance at all.
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "bad :: Box Int -> Maybe (Box Int)\n",
            "bad b = traverse (\\x -> Just x) b\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Traversable"
        )));
    }

    #[test]
    fn a_derived_eq_ord_show_adt_type_checks_end_to_end() {
        let cs = decls(concat!(
            "type Shape\n",
            "  = Circle Float\n",
            "  | Rectangle Float Float\n",
            "  deriving (Eq, Ord, Show)\n",
            "sameShape :: Shape -> Shape -> Bool\n",
            "sameShape a b = a == b\n",
            "orderShapes :: Shape -> Shape -> Bool\n",
            "orderShapes a b = a < b\n",
            "describeShape :: Shape -> String\n",
            "describeShape s = show s\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn deriving_ord_without_eq_still_reports_a_missing_superclass() {
        // Superclass checking applies to a derived instance exactly like a
        // hand-written one -- no special-casing needed in
        // build_instance_table's own two passes.
        let cs = decls("type Shape\n  = Circle Float\n  deriving (Ord)\n");
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::MissingSuperclassInstance { interface, superclass }
                if interface == "Ord" && superclass == "Eq"
        )));
    }

    #[test]
    fn deriving_an_interface_a_hand_written_instance_already_has_is_a_duplicate() {
        let cs = decls(concat!(
            "type Shape\n",
            "  = Circle Float\n",
            "  deriving (Eq)\n",
            "instance Eq Shape where\n",
            "  (==) a b = True\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::DuplicateInstance { interface } if interface == "Eq"
        )));
    }

    #[test]
    fn a_generic_derived_eq_recurses_into_its_own_type_parameter() {
        let cs = decls(concat!(
            "type Box a = Box a\n",
            "  deriving (Eq)\n",
            "sameBox :: Box Int -> Box Int -> Bool\n",
            "sameBox a b = a == b\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_generic_derived_eq_rejects_a_type_argument_with_no_eq_instance() {
        let cs = decls(concat!(
            "type NoEq = NoEq (Int -> Int)\n",
            "type Box a = Box a\n",
            "  deriving (Eq)\n",
            "bad :: Box NoEq -> Box NoEq -> Bool\n",
            "bad a b = a == b\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Eq"
        )));
    }

    #[test]
    fn a_nested_let_over_a_rigid_ord_constrained_parameter_is_not_ambiguous() {
        // A hand-rolled quicksort's `smaller`/`larger`: zero-argument `let`
        // bindings over a comparison against the enclosing rigid `a`. They must
        // not be treated as ambiguous.
        let cs = decls(concat!(
            "quicksort :: Ord a => List a -> List a\n",
            "quicksort xs = case xs of\n",
            "  [] -> []\n",
            "  pivot : rest ->\n",
            "    let\n",
            "        smaller = filter (\\x -> x < pivot) rest\n",
            "        larger = filter (\\x -> x >= pivot) rest\n",
            "    in\n",
            "    quicksort smaller <> (pivot : quicksort larger)\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_parametric_instances_own_requires_resolves_against_a_rigid_given_argument() {
        // `Max a`'s Semigroup instance requires `Ord a`; under `Ord a =>` that
        // requirement must resolve against the rigid `a`'s given interfaces.
        let cs = decls(concat!(
            "type Max a = Max a\n",
            "instance Ord a => Semigroup (Max a) where\n",
            "  (<>) x y = case (x, y) of\n",
            "    (Max a, Max b) -> if a > b then x else y\n",
            "unwrapMax :: Max a -> a\n",
            "unwrapMax m = case m of\n",
            "  Max a -> a\n",
            "maximumOf :: Ord a => a -> List a -> a\n",
            "maximumOf first rest = unwrapMax (foldl (\\acc x -> acc <> Max x) (Max first) rest)\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_recursive_parametric_instance_resolves_its_own_element_constraint() {
        // Self-referential version of the above: `Show (Tree a)`'s body calls
        // `show` on child trees, resolved against the rigid, given-only `a`.
        let cs = decls(concat!(
            "type Tree a = Leaf | Node (Tree a) a (Tree a)\n",
            "instance Show a => Show (Tree a) where\n",
            "  show t = case t of\n",
            "    Leaf -> \".\"\n",
            "    Node l x r -> \"(\" <> show l <> \" \" <> show x <> \" \" <> show r <> \")\"\n",
            "sample :: Tree Int\n",
            "sample = Node (Node Leaf 1 Leaf) 2 (Node Leaf 3 Leaf)\n",
            "sampleShown :: String\n",
            "sampleShown = show sample\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_custom_ord_instance_on_a_record_alias_is_rejected_in_v0() {
        let cs = decls(concat!(
            "type alias Point = { x : Float, y : Float }\n",
            "magnitude :: Point -> Float\n",
            "magnitude p = p.x * p.x + p.y * p.y\n",
            "instance Eq Point where\n",
            "  (==) a b = a.x == b.x && a.y == b.y\n",
            "instance Ord Point where\n",
            "  compare a b = compare (magnitude a) (magnitude b)\n",
            "closerToOrigin :: Point -> Point -> Point\n",
            "closerToOrigin a b = if a < b then a else b\n",
        ));
        let errors = check_module(&cs);
        assert!(
            errors.iter().any(|e| matches!(
                e.kind,
                crate::error::TypeErrorKind::InstanceTargetNotNominal { .. }
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn a_custom_num_instance_on_a_nominal_record_wrapper_type_checks_end_to_end() {
        // A nominal wrapper owns custom arithmetic; its record payload
        // still uses ordinary field access and structural typing.
        let cs = decls(concat!(
            "type Vector2 = Vector2 { x : Float, y : Float }\n",
            "instance Num Vector2 where\n",
            "  (+) (Vector2 a) (Vector2 b) = Vector2 { x = a.x + b.x, y = a.y + b.y }\n",
            "  (-) (Vector2 a) (Vector2 b) = Vector2 { x = a.x - b.x, y = a.y - b.y }\n",
            "  (*) (Vector2 a) (Vector2 b) = Vector2 { x = a.x * b.x, y = a.y * b.y }\n",
            "  negate (Vector2 v) = Vector2 { x = 0.0 - v.x, y = 0.0 - v.y }\n",
            "  abs v = v\n",
            "  signum v = v\n",
            "  fromInteger n = Vector2 { x = fromInteger n, y = fromInteger n }\n",
            "sampleSum :: Vector2\n",
            "sampleSum = Vector2 { x = 1.0, y = 2.0 } + Vector2 { x = 3.0, y = 4.0 }\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_open_row_polymorphic_record_still_cant_take_a_custom_instance() {
        // `HasX a = { a | x : Float }` is a genuinely open row -- there's
        // no fixed, exact shape to match a custom instance's own declared
        // target against. Like closed records, it cannot own a custom
        // instance in v0.
        let cs = decls(concat!(
            "type alias HasX a = { a | x : Float }\n",
            "instance Eq (HasX a) where\n",
            "  (==) p q = p.x == q.x\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.iter().any(
            |e| matches!(&e.kind, crate::error::TypeErrorKind::InstanceTargetNotNominal { interface } if interface == "Eq")
        ), "{errors:?}");
    }

    #[test]
    fn a_declared_float_signature_accepts_an_integer_literal_body() {
        // The exact motivating case: `x :: Float; x = 5` used to be a hard
        // Unify::Mismatch(Float, Int) -- an int literal is now `Num a =>
        // a`, so it unifies with the declared `Float` directly.
        let cs = decls("x :: Float\nx = 5\n");
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_unannotated_integer_literal_still_defaults_to_int() {
        // No signature at all -- must still behave exactly as before this
        // fix (ordinary Int arithmetic), not become ambiguous or silently
        // stay polymorphic.
        let cs = decls("x = 5\ny = x + 1\n");
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_integer_literal_used_as_a_generic_num_argument_still_defaults() {
        // `result = f 1 2`: the literals' shared variable appears in no binding's
        // type, so it is ambiguous and defaults to Int at the end of the module.
        let cs = decls("f :: Eq a => a -> a -> Bool\nf x y = x == y\nresult = f 1 2\n");
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_integer_literal_defaults_to_int_even_inside_a_custom_num_instance() {
        // A custom Num instance elsewhere in the module must not change
        // plain-Int-literal defaulting for code that never touches it.
        let cs = decls(concat!(
            "type Vector2 = Vector2 { x : Float, y : Float }\n",
            "instance Num Vector2 where\n",
            "  (+) (Vector2 a) (Vector2 b) = Vector2 { x = a.x + b.x, y = a.y + b.y }\n",
            "  (-) (Vector2 a) (Vector2 b) = Vector2 { x = a.x - b.x, y = a.y - b.y }\n",
            "  (*) (Vector2 a) (Vector2 b) = Vector2 { x = a.x * b.x, y = a.y * b.y }\n",
            "  negate (Vector2 v) = Vector2 { x = 0.0 - v.x, y = 0.0 - v.y }\n",
            "  abs v = v\n",
            "  signum v = v\n",
            "  fromInteger n = Vector2 { x = fromInteger n, y = fromInteger n }\n",
            "plainSum = 1 + 2\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_integer_literal_with_no_num_instance_for_its_pinned_type_is_a_real_error() {
        // `f :: Bool; f = 5` -- Bool has no Num instance, so this must
        // still be a genuine error (NoInstance("Num"), not a silently
        // accepted Unify success now that literals are polymorphic).
        let cs = decls("f :: Bool\nf = 5\n");
        let errors = check_module(&cs);
        assert!(
            errors.iter().any(
                |e| matches!(&e.kind, crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Num")
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn map_fromlist_builds_a_map_end_to_end() {
        // collections/build-map.knot's own motivating case: Map.fromList
        // didn't exist at all (UnboundValue).
        let cs = decls(concat!(
            "populations :: Map.Map String Int\n",
            "populations = Map.fromList [(\"Tokyo\", 37400000), (\"Delhi\", 30290000)]\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn map_get_insert_and_empty_type_check_end_to_end() {
        // collections/word-count-attempt.knot's own motivating case:
        // Map.get/Map.insert/Map.empty didn't exist at all.
        let cs = decls(concat!(
            "countWord :: String -> Map.Map String Int -> Map.Map String Int\n",
            "countWord word counts =\n",
            "  case Map.get word counts of\n",
            "    Just n -> Map.insert word (n + 1) counts\n",
            "    Nothing -> Map.insert word 1 counts\n",
            "countAll :: List String -> Map.Map String Int\n",
            "countAll words = foldl (\\counts w -> countWord w counts) Map.empty words\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn map_key_type_needs_ord() {
        // A key type with no Ord instance must be rejected -- Map's
        // own key-comparing functions all declare `Ord k =>`, not silently
        // accept anything. `NoEqType` is a concrete nominal type here (not
        // a bare lowercase type variable), so it resolves to an ordinary
        // `Structure::App`, not a rigid variable -- the obligation is
        // checked against the real instance table, reporting a plain
        // `NoInstance`, not `NoInstanceForRigid`.
        let cs = decls(concat!(
            "type NoEqType = NoEqType Float\n",
            "bad :: NoEqType -> Map.Map NoEqType Int -> Maybe Int\n",
            "bad key m = Map.get key m\n",
        ));
        let errors = check_module(&cs);
        assert!(
            errors.iter().any(
                |e| matches!(&e.kind, crate::error::TypeErrorKind::NoInstance { interface, .. } if interface == "Ord")
            ),
            "{errors:?}"
        );
    }

    #[test]
    fn an_instance_methods_local_let_over_an_integral_rigid_is_not_ambiguous() {
        // A `let` inside an instance method body computing intermediate values
        // with `div`/`mod` must not be treated as ambiguous.
        let cs = decls(concat!(
            "type Money a = Money { cents : a }\n",
            "instance (Integral a, Show a) => Show (Money a) where\n",
            "  show (Money m) =\n",
            "    let\n",
            "        dollars = div m.cents 100\n",
            "        remainder = mod m.cents 100\n",
            "    in\n",
            "    \"$\" <> show dollars <> \".\" <> show remainder\n",
        ));
        let errors = check_module(&cs);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn check_module_with_warnings_reports_a_non_exhaustive_case_but_no_type_error() {
        // known_gaps/non-exhaustive-case-not-flagged.knot's own motivating
        // case: a case missing an entire constructor arm used to produce
        // zero diagnostics of any kind. Now a Warning -- but still not a
        // TypeError, so a caller that only looks at check_module's own
        // Vec<TypeError> (all ~30 of check_module's own existing callers)
        // correctly keeps seeing this as a clean type-check.
        let cs = decls(concat!(
            "type Shape = Circle Float | Square Float | Triangle Float Float Float\n",
            "area :: Shape -> Float\n",
            "area shape = case shape of\n",
            "  Circle r -> 3.14159 * r * r\n",
            "  Square s -> s * s\n",
        ));
        let (errors, warnings) = check_module_with_warnings(&cs);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            warnings[0].kind,
            crate::exhaustiveness::WarningKind::NonExhaustiveMatch
        );
    }
}
