use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
fn snapshot(source: &str) -> ProjectSnapshot {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "knot-revisions-{}-{}.knot",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    ProjectSnapshot::load(
        &path,
        &[],
        &Default::default(),
        Some(format!("{source}\ntype GuardedRequest a = GuardedRequest a (a -> Bool)\ninstance Eq a => Constriction (GuardedRequest a) a where\n  fits (GuardedRequest expected guard) actual = expected == actual && guard actual\n")),
        &Default::default(),
    )
    .unwrap()
}
fn setup(source: &str) -> (ProjectSnapshot, knot_core::Program, Probes) {
    let original = snapshot(source);
    let library = crate::libraries::Libraries::load(None).unwrap().0.core;
    let context = Arc::new(ProbeContext::new(&original, &library).unwrap());
    let probes = Probes::from_context(context, EditLimits::default());
    (original, library, probes)
}
fn observe(probes: &mut Probes, value: &TypedValue) -> Data {
    let name = probes.reference(value).unwrap();
    probes.declarations.clear();
    let (result, _) = probes.evaluate_lazy(&format!("({name}) ()"), None).unwrap();
    probes.decode_live(&result).unwrap()
}

fn truth(probes: &mut Probes, value: &TypedValue) -> bool {
    match probes.decode_live(value).unwrap() {
        Data::Constructor(name, _) if name.module == ["Prelude"] => name.name == "True",
        other => panic!("expected Bool, got {other:?}"),
    }
}

#[test]
fn candidate_context_reuses_exact_imports_but_rebuilds_changed_dependencies() {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "knot-context-imports-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let entry = dir.join("Main.knot");
    let dependency = dir.join("Other.knot");
    let source = "module Main exposing (..)\nimport Other\ninfixl 5 (+++)\n(+++) :: Int -> Int -> Int\n(+++) a b = a - b\nvalue :: Int\nvalue = Other.number + (10 +++ 3 +++ 1)\n";
    std::fs::write(&entry, source).unwrap();
    std::fs::write(
        &dependency,
        "module Other exposing (number)\nnumber :: Int\nnumber = 2\nhidden :: Int\nhidden = 41\n",
    )
    .unwrap();
    let load = |previous| {
        ProjectSnapshot::load_reusing(
            &entry,
            &[],
            &Default::default(),
            None,
            &Default::default(),
            previous,
        )
        .unwrap()
    };
    let library = crate::libraries::Libraries::load(None).unwrap().0.core;
    let original = load(None);
    let context = ProbeContext::new(&original, &library).unwrap();
    let evaluate = |context, expression| {
        let mut probes = Probes::from_context(Arc::new(context), EditLimits::default());
        let (result, _) = probes.evaluate_lazy(expression, None).unwrap();
        probes.decode_live(&result).unwrap()
    };
    // Entry fixity changes must still be parsed and linked; imported private
    // bindings remain available to internal queries through reused interfaces.
    std::fs::write(&entry, source.replace("infixl", "infixr")).unwrap();
    let candidate = load(Some(&original));
    let reused = ProbeContext::new_reusing(&candidate, &library, Some(&context)).unwrap();
    assert!(Arc::ptr_eq(
        &context.dependency_base.shared_program(),
        &reused.dependency_base.shared_program()
    ));
    assert!(Arc::ptr_eq(&context.imports.interfaces()[0], &reused.imports.interfaces()[0]));
    assert!(!Arc::ptr_eq(
        context.imports.interfaces().last().unwrap(),
        reused.imports.interfaces().last().unwrap()
    ));
    assert_ne!(
        context.imports.interfaces().last().unwrap().fixities,
        reused.imports.interfaces().last().unwrap().fixities
    );
    assert_eq!(
        evaluate(reused, "(Main.value, Other.hidden)"),
        Data::Tuple(vec![
            Data::Literal(knot_core::Literal::Int(10)),
            Data::Literal(knot_core::Literal::Int(41))
        ])
    );

    // Changing an imported implementation must rebuild linked dependencies even
    // when its exported type is unchanged.
    std::fs::write(
        &dependency,
        "module Other exposing (number)\nnumber :: Int\nnumber = 20\nhidden :: Int\nhidden = 99\n",
    )
    .unwrap();
    let changed = load(Some(&candidate));
    let rebuilt = ProbeContext::new_reusing(&changed, &library, Some(&context)).unwrap();
    assert!(!Arc::ptr_eq(
        &context.dependency_base.shared_program(),
        &rebuilt.dependency_base.shared_program()
    ));
    assert!(!Arc::ptr_eq(&context.imports.interfaces()[0], &rebuilt.imports.interfaces()[0]));
    assert_eq!(
        evaluate(rebuilt, "(Main.value, Other.hidden)"),
        Data::Tuple(vec![
            Data::Literal(knot_core::Literal::Int(28)),
            Data::Literal(knot_core::Literal::Int(99))
        ])
    );
    assert_eq!(
        evaluate(context, "Main.value"),
        Data::Literal(knot_core::Literal::Int(8))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn validate_keep(probes: &mut Probes, request: &TypedValue) -> Result<(), EditError> {
    let name = probes.reference(request)?;
    probes.flush_validation(&mut vec![(
        format!("current = Main.keep\nrequest = {name}\n"),
        "fits request current".into(),
    )])
}

#[test]
fn validation_witnesses_follow_dependencies_and_exact_request_handles() {
    let source = "module Main exposing (..)\nkeep :: Int\nkeep = 5\nother :: Int\nother = 0\n";
    let (original, library, mut probes) = setup(source);
    let (request, _) = probes.evaluate_lazy("Just Main.keep", None).unwrap();
    validate_keep(&mut probes, &request).unwrap();
    let candidate = snapshot(&source.replace("other = 0", "other = 1"));
    probes
        .install_candidate(&original, &candidate, &library)
        .unwrap();
    let sequence = probes.live.as_ref().unwrap().sequence;
    validate_keep(&mut probes, &request).unwrap();
    assert_eq!(probes.validation_cache_hits(), 1);
    assert_eq!(probes.live.as_ref().unwrap().sequence, sequence);

    let changed = snapshot(&source.replace("keep = 5", "keep = 6"));
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    assert!(matches!(
        validate_keep(&mut probes, &request),
        Err(EditError::ValidationFailed)
    ));
    assert_eq!(probes.validation_cache_hits(), 1);
    let (six, _) = probes.evaluate_lazy("Just Main.keep", None).unwrap();
    validate_keep(&mut probes, &six).unwrap();
    assert_eq!(
        probes.validation_cache_hits(),
        1,
        "different arguments cannot reuse success"
    );
    probes.limits.traversal.queries = probes.count;
    assert!(matches!(
        validate_keep(&mut probes, &six),
        Err(EditError::Unsupported(_))
    ));
}

#[test]
fn validation_cache_retains_distinct_argument_sets_for_the_same_checked_code() {
    let (_, _, mut probes) = setup("module Main exposing (..)\nkeep :: Int\nkeep = 5\n");
    let (first, _) = probes.evaluate_lazy("Just Main.keep", None).unwrap();
    let (second, _) = probes.evaluate_lazy("Just (Main.keep + 0)", None).unwrap();
    assert!(!first.same_thunk(&second));
    validate_keep(&mut probes, &first).unwrap();
    validate_keep(&mut probes, &second).unwrap();
    validate_keep(&mut probes, &first).unwrap();
    assert_eq!(probes.validation_cache_hits(), 1);
}

#[test]
fn validation_proofs_include_branches_and_selected_dictionaries() {
    let source = "module Main exposing (..)\nswitch = True\nkeep :: Int\nkeep = if switch then 5 else other\nother :: Int\nother = 6\n";
    let (original, library, mut probes) = setup(source);
    let (request, _) = probes.evaluate_lazy("Just Main.keep", None).unwrap();
    validate_keep(&mut probes, &request).unwrap();
    let candidate = snapshot(&source.replace("other = 6", "other = 7"));
    probes
        .install_candidate(&original, &candidate, &library)
        .unwrap();
    validate_keep(&mut probes, &request).unwrap();
    assert_eq!(
        probes.validation_cache_hits(),
        0,
        "even the untaken branch is a static dependency"
    );
    let changed = snapshot(&source.replace("switch = True", "switch = False"));
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    assert!(matches!(
        validate_keep(&mut probes, &request),
        Err(EditError::ValidationFailed)
    ));

    let source = "module Main exposing (..)\ntype Token = Token Int\ninstance Eq Token where\n  (==) _ _ = True\nkeep = Token 5\n";
    let (original, library, mut probes) = setup(source);
    let (request, _) = probes.evaluate_lazy("Just (Main.Token 5)", None).unwrap();
    validate_keep(&mut probes, &request).unwrap();
    let changed = snapshot(&source.replace("= True", "= False"));
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    assert!(matches!(
        validate_keep(&mut probes, &request),
        Err(EditError::ValidationFailed)
    ));
    assert_eq!(probes.validation_cache_hits(), 0);
}

#[test]
fn validation_witnesses_preserve_old_request_captures_and_fall_back_on_proof_limits() {
    let source = "module Main exposing (..)\nkeep :: Int\nkeep = 5\nlimit :: Int\nlimit = 5\n";
    let (original, library, mut probes) = setup(source);
    let (request, _) = probes
        .evaluate_lazy(
            "Main.GuardedRequest Main.limit (\\x -> x == Main.limit)",
            None,
        )
        .unwrap();
    validate_keep(&mut probes, &request).unwrap();
    let changed = snapshot(&source.replace("limit = 5", "limit = 6"));
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    validate_keep(&mut probes, &request).unwrap();
    assert_eq!(
        probes.validation_cache_hits(),
        1,
        "original request still captures limit = 5"
    );
    let (new_request, _) = probes
        .evaluate_lazy(
            "Main.GuardedRequest Main.limit (\\x -> x == Main.limit)",
            None,
        )
        .unwrap();
    assert!(matches!(
        validate_keep(&mut probes, &new_request),
        Err(EditError::ValidationFailed)
    ));

    let (_, _, mut probes) = setup(source);
    probes.limits.traversal.dependency_nodes = 0;
    let (request, _) = probes.evaluate_lazy("Just Main.keep", None).unwrap();
    validate_keep(&mut probes, &request).unwrap();
    validate_keep(&mut probes, &request).unwrap();
    assert_eq!(probes.validation_cache_hits(), 0);
}

#[test]
fn call_plans_keep_concrete_types_revision_identity_and_query_limits() {
    let source = "module Main exposing (..)\ntype Token = Token Int\ntoken = Token 1\npoly :: Num a => a\npoly = 1\nidentity :: a -> a\nidentity x = x\n";
    let (original, library, mut probes) = setup(source);
    let global = |name: &str| GlobalName {
        module: vec!["Main".into()],
        name: name.into(),
    };
    assert_eq!(
        probes.monomorphic_global(&global("token")),
        Some(&Ty::Named("Main.Token".into(), vec![]))
    );
    assert_eq!(probes.monomorphic_global(&global("poly")), None);
    assert_eq!(probes.monomorphic_global(&global("identity")), None);
    let body = "value :: Int\nvalue = undefined\nrequest :: Maybe Int\nrequest = undefined\nframe = (\\fun arg -> (fun arg, arg)) Main.identity value\n";
    let first = probes.call_plan(body, 1).unwrap();
    let again = probes.call_plan(body, 1).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(first.arguments, [Ty::Named("Int".into(), vec![])]);
    let float = probes.call_plan(&body.replace("Int", "Float"), 1).unwrap();
    assert!(!Arc::ptr_eq(&first, &float));
    assert_eq!(float.arguments, [Ty::Named("Float".into(), vec![])]);
    probes.evaluate_lazy("Main.token", None).unwrap();
    let changed = snapshot(&source.replace("Token 1", "Token 2"));
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    let candidate = probes.call_plan(body, 1).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &candidate),
        "candidate metadata must use its own checked context"
    );
    probes.limits.traversal.queries = probes.count;
    assert!(matches!(
        probes.call_plan(body, 1),
        Err(EditError::Unsupported(_))
    ));
}

#[test]
fn metadata_reuses_argument_types_without_reusing_argument_names_or_user_syntax() {
    let (_, _, mut probes) = setup("module Main exposing (..)\nprogram = 1\n");
    let int = Ty::Named("Int".into(), vec![]);
    let (one, _) = probes.evaluate_lazy("1", Some(&int)).unwrap();
    let (two, _) = probes.evaluate_lazy("2", Some(&int)).unwrap();
    let first = probes.reference(&one).unwrap();
    let second = probes.reference(&two).unwrap();
    probes.declarations.clear();
    let a = probes.check(&format!("probe = ({first})\n")).unwrap();
    let b = probes.check(&format!("probe = ({second})\n")).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "only retained argument names changed");
    let string_a = probes.check(&format!("probe = \"{first}\"\n")).unwrap();
    let string_b = probes.check(&format!("probe = \"{second}\"\n")).unwrap();
    assert!(!Arc::ptr_eq(&string_a, &string_b), "strings remain exact");
    let local_a = probes
        .check(&format!("probe = \\{first} -> {first}\n"))
        .unwrap();
    let local_b = probes
        .check(&format!("probe = \\{second} -> {second}\n"))
        .unwrap();
    assert!(
        !Arc::ptr_eq(&local_a, &local_b),
        "local binders are not generated references"
    );
    let (boolean, _) = probes.evaluate_lazy("True", None).unwrap();
    let reference = probes.reference(&boolean).unwrap();
    let different = probes.check(&format!("probe = ({reference})\n")).unwrap();
    assert!(!Arc::ptr_eq(&different, &a), "argument types remain exact");
}

#[test]
fn prepared_calls_share_code_not_requests_or_captured_environments() {
    let source = "module Main exposing (..)\nlimit :: Int\nlimit = 1\n";
    let (_, _, mut probes) = setup(source);
    let int = Ty::Named("Int".into(), vec![]);
    let bool_ = Ty::Named("Bool".into(), vec![]);
    let (one, _) = probes.evaluate_lazy("1", Some(&int)).unwrap();
    let (two, _) = probes.evaluate_lazy("2", Some(&int)).unwrap();
    let (first, _) = probes
        .evaluate_lazy(
            "Main.GuardedRequest Main.limit (\\x -> x == Main.limit)",
            None,
        )
        .unwrap();
    let (second, _) = probes
        .evaluate_lazy(
            "Main.GuardedRequest (Main.limit + 1) (\\x -> x == Main.limit + 1)",
            None,
        )
        .unwrap();
    let (bad, _) = probes
        .evaluate_lazy("Main.GuardedRequest Main.limit (\\_ -> False)", None)
        .unwrap();
    let mut sequence = None;
    for (request, value, expected) in [
        (&first, &one, true),
        (&second, &one, false),
        (&second, &two, true),
        (&bad, &one, false),
    ] {
        let result = probes
            .call_typed("Prelude.fits", &[request.clone(), value.clone()], &bool_)
            .unwrap();
        assert_eq!(truth(&mut probes, &result), expected);
        let actual = probes.live.as_ref().unwrap().sequence;
        assert_eq!(
            *sequence.get_or_insert(actual),
            actual,
            "arguments must not recompile the callable"
        );
    }
    // A prepared application needs no more checked-source queries, but its
    // execution still obeys the cumulative runtime budget and cancellation.
    let queries = probes.count;
    probes.limits.traversal.queries = probes.count;
    let result = probes
        .call_typed("Prelude.fits", &[first.clone(), one.clone()], &bool_)
        .unwrap();
    assert!(truth(&mut probes, &result));
    assert_eq!(probes.count, queries);
    probes.limits.runtime.steps = probes.usage.steps;
    let exhausted = probes
        .call_typed("Prelude.fits", &[second, one.clone()], &bool_)
        .and_then(|value| probes.decode_live(&value));
    assert!(
        matches!(exhausted, Err(EditError::Runtime(_))),
        "{exhausted:?}"
    );
    probes
        .limits
        .runtime
        .cancellation
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        probes.call_typed("Prelude.fits", &[first, one], &bool_),
        Err(EditError::Runtime(twine::Outcome::Cancelled))
    ));
}

#[test]
fn prepared_validator_refreshes_candidate_dictionaries_but_preserves_old_requests() {
    let source = "module Main exposing (..)\ntype Token = Token Int\ninstance Eq Token where\n  (==) _ _ = True\nlimit :: Int\nlimit = 1\n";
    let (original, library, mut probes) = setup(source);
    let bool_ = Ty::Named("Bool".into(), vec![]);
    let (request, _) = probes.evaluate_lazy("Just (Main.Token 1)", None).unwrap();
    let (value, _) = probes.evaluate_lazy("Main.Token 2", None).unwrap();
    let (captured, _) = probes
        .evaluate_lazy(
            "Main.GuardedRequest Main.limit (\\x -> x == Main.limit)",
            None,
        )
        .unwrap();
    let (one, _) = probes.evaluate_lazy("Main.limit", None).unwrap();
    let result = probes
        .call_typed("Prelude.fits", &[request.clone(), value.clone()], &bool_)
        .unwrap();
    assert!(truth(&mut probes, &result));
    let changed = snapshot(
        &source
            .replace("= True", "= False")
            .replace("limit = 1", "limit = 2"),
    );
    probes
        .install_candidate(&original, &changed, &library)
        .unwrap();
    let result = probes
        .call_typed("Prelude.fits", &[request, value], &bool_)
        .unwrap();
    assert!(
        !truth(&mut probes, &result),
        "use the candidate Eq dictionary"
    );
    let (two, _) = probes.evaluate_lazy("Main.limit", None).unwrap();
    for (value, expected) in [(one, true), (two, false)] {
        let result = probes
            .call_typed("Prelude.fits", &[captured.clone(), value], &bool_)
            .unwrap();
        assert_eq!(
            truth(&mut probes, &result),
            expected,
            "the original guard retains limit = 1"
        );
    }
}
#[test]
fn revisions_preserve_lazy_closures_dictionaries_and_cumulative_usage() {
    let source = "module Main exposing (..)\ntype Token = Token Int\ninstance Eq Token where\n  (==) _ _ = True\nnumber :: Int\nnumber = 1\n";
    let (original, library, mut probes) = setup(source);
    let expression = r"\() -> (Main.number, Main.Token 1 == Main.Token 1)";
    let (old, _) = probes.evaluate_lazy(expression, None).unwrap();
    let first = snapshot(
        &source
            .replace("number = 1", "number = 4")
            .replace("= True", "= False"),
    );
    probes
        .install_candidate(&original, &first, &library)
        .unwrap();
    let (middle, _) = probes.evaluate_lazy(expression, None).unwrap();
    let used = probes.usage;
    let second = snapshot(&source.replace("number = 1", "number = 7"));
    probes
        .install_candidate(&original, &second, &library)
        .unwrap();
    let (new, _) = probes.evaluate_lazy(expression, None).unwrap();
    for (closure, number, truth) in [(&old, 1, "True"), (&middle, 4, "False"), (&new, 7, "True")] {
        assert_eq!(
            observe(&mut probes, closure),
            Data::Tuple(vec![
                Data::Literal(knot_core::Literal::Int(number)),
                Data::Constructor(
                    GlobalName {
                        module: vec!["Prelude".into()],
                        name: truth.into()
                    },
                    vec![]
                ),
            ])
        );
    }
    let retained_bytes = probes.live.as_ref().unwrap().source_bytes;
    probes
        .install_candidate(&original, &original, &library)
        .unwrap();
    let (restored, _) = probes.evaluate_lazy(expression, None).unwrap();
    assert_eq!(observe(&mut probes, &restored), observe(&mut probes, &old));
    assert!(probes.live.as_ref().unwrap().source_bytes >= retained_bytes);
    // A second attempt gets fresh executable names while old closures survive.
    probes
        .install_candidate(&original, &first, &library)
        .unwrap();
    let (retried, _) = probes.evaluate_lazy(expression, None).unwrap();
    assert_eq!(
        observe(&mut probes, &retried),
        observe(&mut probes, &middle)
    );
    assert!(probes.usage.steps > used.steps);
    assert!(probes.usage.heap_entries > used.heap_entries);
    assert_eq!(probes.live.as_ref().unwrap().revisions, 3);
}
#[test]
fn failed_revision_limits_cancellation_and_abi_changes_leave_live_values_intact() {
    let source = "module Main exposing (..)\nnumber :: Int\nnumber = 1\n";
    let (original, library, mut probes) = setup(source);
    let (old, _) = probes.evaluate_lazy(r"\() -> Main.number", None).unwrap();
    let next = snapshot(&source.replace("number = 1", "number = 2"));
    probes.limits.traversal.candidate_revisions = 0;
    assert!(matches!(
        probes.install_candidate(&original, &next, &library),
        Err(EditError::Unsupported(_))
    ));
    probes.limits.traversal.candidate_revisions = 2;
    let allowance = probes.limits.traversal.compiled_bytes;
    probes.limits.traversal.compiled_bytes = probes.live.as_ref().unwrap().source_bytes
        + next.sources().values().map(String::len).sum::<usize>()
        - 1;
    assert!(matches!(
        probes.install_candidate(&original, &next, &library),
        Err(EditError::SourceTooLarge)
    ));
    probes.limits.traversal.compiled_bytes = allowance;
    probes
        .limits
        .runtime
        .cancellation
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        probes.install_candidate(&original, &next, &library),
        Err(EditError::Runtime(twine::Outcome::Cancelled))
    ));
    probes
        .limits
        .runtime
        .cancellation
        .store(false, Ordering::Relaxed);
    let incompatible = snapshot(
        &source
            .replace(":: Int", ":: String")
            .replace("= 1", "= \"x\""),
    );
    assert!(matches!(
        probes.install_candidate(&original, &incompatible, &library),
        Err(EditError::Check(_))
    ));
    assert_eq!(probes.live.as_ref().unwrap().revisions, 0);
    assert_eq!(
        observe(&mut probes, &old),
        Data::Literal(knot_core::Literal::Int(1))
    );
    probes
        .install_candidate(&original, &next, &library)
        .unwrap();
    let (new, _) = probes.evaluate_lazy(r"\() -> Main.number", None).unwrap();
    assert_eq!(
        observe(&mut probes, &new),
        Data::Literal(knot_core::Literal::Int(2))
    );
    assert_eq!(
        observe(&mut probes, &old),
        Data::Literal(knot_core::Literal::Int(1))
    );
}

#[test]
fn an_oversized_helper_keeps_the_checked_route_prefix() {
    let (_, _, mut cold) = setup("module Main exposing (..)\nprogram = 1\n");
    let context = cold.context.clone();
    let int = Ty::Named("Int".into(), vec![]);
    cold.enable_route().unwrap();
    cold.evaluate_lazy("1", Some(&int)).unwrap();
    cold.evaluate_lazy("2", Some(&int)).unwrap();
    let large = format!("40 --{}", "x".repeat(256 * 1024));
    cold.evaluate_lazy(&large, Some(&int)).unwrap();
    cold.finish_recording();
    assert!(context.route.get().is_none());
    cold.publish_route();

    let mut warm = Probes::from_context(context, EditLimits::default());
    warm.enable_route().unwrap();
    for (expression, expected) in [("2", 2), (large.as_str(), 40)] {
        let (value, _) = warm.evaluate_lazy(expression, Some(&int)).unwrap();
        assert_eq!(
            warm.decode_live(&value).unwrap(),
            Data::Literal(knot_core::Literal::Int(expected))
        );
        assert_eq!(warm.route_hits, 1);
    }
}
