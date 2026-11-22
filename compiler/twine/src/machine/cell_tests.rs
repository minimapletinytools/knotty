//! Representation-boundary checks. Explicit sharing here tests the cell/use
//! contract independently of which expressions the optimizer currently admits.
use super::*;
use knot_checker::check::{check_library_program, check_program};
fn name(n: &str) -> GlobalName {
    GlobalName {
        module: vec!["Test".into()],
        name: n.into(),
    }
}
fn program(source: &str) -> Program {
    let parsed = knot_syntax::parse(&format!("module Test exposing (..)\n{source}")).unwrap();
    let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
    knot_core::lower(&[
        check_library_program("Prelude").unwrap(),
        check_program(&canonical).unwrap(),
    ])
}
fn handle(s: &Session, t: ThunkId) -> ThunkHandle {
    ThunkHandle {
        session: s.id,
        thunk: t.0,
    }
}
fn global(s: &Session, n: &str) -> ThunkId {
    s.globals[&Global::Value(name(n))]
}
fn scoped_uses(s: &mut Session, owner: ThunkId) -> [ThunkId; 2] {
    let local = *s
        .program
        .local_names
        .iter()
        .find(|(_, n)| *n == "unused")
        .unwrap()
        .0;
    let origin = s.thunks[owner.0].origin;
    [10, 20].map(|n| {
        let origin = if n == 10 {
            origin
        } else {
            s.thunks[global(s, "scope").0].origin
        };
        let value = s.ready(Value::Lit(Literal::Int(n)), origin);
        let env = s.env(Some(0), Bindings::One(local, value), Some(origin));
        s.shared_use(s.cell_id(owner), origin, s.envs[env].activation)
    })
}
fn unused(s: &mut Session, use_id: ThunkId) -> i64 {
    let bindings = s.activation_bindings(s.thunks[use_id.0].activation);
    let value = s
        .force(bindings.iter().find(|b| b.name == "unused").unwrap().handle)
        .unwrap();
    let ValueView::Literal(Literal::Int(n)) = s.view(value).unwrap() else {
        panic!("expected Int")
    };
    *n
}
const SOURCE: &str = "scope unused = unused\ncost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\nanswer :: Int\nanswer = cost 300\n";

#[cfg(feature = "incremental")]
#[test]
fn expensive_calls_keep_identity_when_cheap_factories_are_not_retained() {
    for record in [false, true] {
        let factory = if record {
            "make offset = { run = \\n -> let loop k = if k == 0 then offset else 1 + loop (k - 1) in loop n }"
        } else {
            "make offset n = let loop k = if k == 0 then offset else 1 + loop (k - 1) in loop n"
        };
        let call = if record {
            "(make offset).run input"
        } else {
            "make offset input"
        };
        let p = program(&format!("{factory}\noffset :: Int\noffset = 7\ninput :: Int\ninput = 200\nfirst = {call}\nsecond = {call}\n"));
        let mut s = Session::new(Arc::new(p), Limits::default());
        // Test-only admission probe: there are no protected value kinds.
        s.limits.cache_minimum_work = 64;
        let first = s.evaluate(&name("first")).unwrap();
        assert_eq!(
            s.view(first).unwrap(),
            ValueView::Literal(&Literal::Int(207))
        );
        assert!(s.memo.costs().all(|cost| cost.inclusive >= 64));
        // Retained expensive calls must not secretly rely on permanent cheap
        // expression/capture indexes to recreate their execution identities.
        s.memo.forget_non_call_indexes();
        let before = s.stats.steps;
        let second = s.evaluate(&name("second")).unwrap();
        let warm = s.stats.steps - before;
        assert_eq!(
            s.view(second).unwrap(),
            ValueView::Literal(&Literal::Int(207))
        );
        assert!(
            warm * 10 < before,
            "record={record}: cold={before}, warm={warm}"
        );
        eprintln!("factory record={record}: cold={before}, warm={warm}");
    }
}

#[cfg(feature = "incremental")]
#[test]
fn same_execution_inputs_do_not_hide_different_unused_inspection_bindings() {
    let p = program("make :: Int -> Int -> Int -> { value : Int }\nmake unused kept x = { value = kept + x }\none :: Int\none = 1\ntwo :: Int\ntwo = 2\nkept :: Int\nkept = 7\ninput :: Int\ninput = 3\nf = make one kept\ng = make two kept\nfirst = f input\nsecond = g input\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    let f = s.evaluate(&name("f")).unwrap();
    let g = s.evaluate(&name("g")).unwrap();
    let argument = s.cell_id(global(&s, "input"));
    assert_eq!(s.call_key(f.value, argument), s.call_key(g.value, argument));
    assert!(!s.call_scope_compatible(f.value, g.value));
    let a = s.evaluate(&name("first")).unwrap();
    let b = s.evaluate(&name("second")).unwrap();
    assert_ne!(a, b);
    for (value, expected) in [(a, 1), (b, 2)] {
        let child = s.children(value, 0, 1).unwrap()[0].handle;
        let activation = s.thunks[child.thunk].activation;
        let unused = s
            .activation_bindings(activation)
            .into_iter()
            .find(|b| b.name == "unused")
            .unwrap();
        let actual = s.force(unused.handle).unwrap();
        assert_eq!(
            s.view(actual).unwrap(),
            ValueView::Literal(&Literal::Int(expected))
        );
        let actual = s.force(child).unwrap();
        assert_eq!(
            s.view(actual).unwrap(),
            ValueView::Literal(&Literal::Int(10))
        );
    }
}

#[cfg(feature = "incremental")]
#[test]
fn expensive_call_support_survives_an_edit_without_the_cheap_record_result() {
    let common = "make offset = { run = \\n -> let loop k = if k == 0 then offset else 1 + loop (k - 1) in loop n }\noffset :: Int\noffset = 7\ninput :: Int\ninput = 200\n";
    let old_program = program(&format!("{common}before = (make offset).run input\n"));
    let mut old = Session::new(Arc::new(old_program), Limits::default());
    old.limits.cache_minimum_work = 64;
    old.evaluate(&name("before")).unwrap();
    assert!(old.memo.costs().all(|cost| cost.inclusive >= 64));
    // Shift executable/local IDs and remove the old result root. Callee support
    // must be re-established from checked code and already-mapped input cells.
    let new_program = Arc::new(program(&format!(
        "identity x = x\n{common}before = 0\nafter = (make offset).run input\n"
    )));
    let mut next = Session::new(new_program.clone(), Limits::default());
    next.limits.cache_minimum_work = 64;
    let reused = next.reuse_from(&old);
    let value = next.evaluate(&name("after")).unwrap();
    assert_eq!(
        next.view(value).unwrap(),
        ValueView::Literal(&Literal::Int(207))
    );
    let mut fresh = Session::new(new_program, Limits::default());
    fresh.evaluate(&name("after")).unwrap();
    eprintln!(
        "transferred factory: reused={}, fresh={}, applications={}",
        next.stats.steps, fresh.stats.steps, reused.applications
    );
    assert!(reused.applications > 0);
    assert!(next.stats.steps * 10 < fresh.stats.steps);
}

#[cfg(feature = "compact-captures")]
#[test]
fn compact_execution_inputs_preserve_full_unused_inspection_scope() {
    let p = program("make :: Int -> Int -> Int -> (Int, Int)\nmake unused kept x = (kept, x)\nbad :: Int\nbad = error \"unused\"\nf = make bad 7\nanswer = f 3\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    let f = s.evaluate(&name("f")).unwrap();
    let Value::Closure(_, _, capture) = &s.values[f.value] else {
        panic!("closure")
    };
    let inputs = capture.inputs.as_ref().expect("prepared captures");
    let names: Vec<_> = inputs
        .iter()
        .map(|(local, _)| s.program.local_names.get(local).unwrap().as_str())
        .collect();
    assert!(names.contains(&"kept"));
    assert!(!names.contains(&"unused"));
    let scope = s.envs[capture.scope].activation;
    let visible = s.activation_bindings(scope);
    assert!(visible.iter().any(|binding| binding.name == "unused"));
    assert!(visible.iter().any(|binding| binding.name == "kept"));
    let answer = s.evaluate(&name("answer")).unwrap();
    let children = s.children(answer, 0, 2).unwrap();
    for (child, expected) in children.iter().zip([7, 3]) {
        let value = s.force(child.handle).unwrap();
        assert_eq!(
            s.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(expected))
        );
    }
    assert!(matches!(
        s.thunks[global(&s, "bad").0].state,
        ThunkState::Pending(_)
    ));
    eprintln!(
        "capture layout: Env={} Value={} Thunk={} Frame={}",
        std::mem::size_of::<Env>(),
        std::mem::size_of::<Value>(),
        std::mem::size_of::<Thunk>(),
        std::mem::size_of::<Frame>()
    );
}

#[test]
fn shared_pending_work_retains_each_uses_scope_and_rolls_back_by_owner() {
    let mut s = Session::new(Arc::new(program(SOURCE)), Limits::default());
    let owner = global(&s, "answer");
    let [a, b] = scoped_uses(&mut s, owner);
    assert_ne!(a, b);
    assert_eq!(s.cell_id(a), s.cell_id(b));
    assert_ne!(s.thunks[a.0].origin, s.thunks[b.0].origin);
    for use_id in [a, b] {
        let use_ = &s.thunks[use_id.0];
        let allocations = s.inspect_node(use_.origin.node, Some(use_.activation));
        assert!(allocations.iter().any(|a| a.handle == handle(&s, use_id)));
    }
    assert_eq!(unused(&mut s, a), 10);
    assert_eq!(unused(&mut s, b), 20);
    let handles = [handle(&s, a), handle(&s, b)];
    let before = s.heap_entries();
    assert_eq!(
        s.inspect(
            handles[0],
            InspectionBudget {
                steps: 80,
                ..Default::default()
            }
        )
        .unwrap(),
        Inspection::Truncated
    );
    assert_eq!(s.heap_entries(), before);
    for handle in handles {
        assert_eq!(s.inspection_state(handle).unwrap(), Inspection::Unevaluated);
    }
    assert!(matches!(s.thunks[a.0].state, ThunkState::Shared(_)));
    assert_eq!(
        s.inspect(
            handles[0],
            InspectionBudget {
                cancellation: Arc::new(AtomicBool::new(true)),
                ..Default::default()
            }
        )
        .unwrap(),
        Inspection::Cancelled
    );
    assert_eq!(s.heap_entries(), before);
    assert_eq!(
        s.inspection_state(handles[1]).unwrap(),
        Inspection::Unevaluated
    );

    let value = s.force(handles[1]).unwrap();
    assert_eq!(s.force(handles[0]).unwrap(), value);
    assert_eq!(
        s.view(value).unwrap(),
        ValueView::Literal(&Literal::Int(300))
    );
    assert_eq!(unused(&mut s, a), 10);
    assert_eq!(unused(&mut s, b), 20);
    assert_eq!(
        s.observed_data_key(handles[0], 8, 64).unwrap(),
        s.observed_data_key(handles[1], 8, 64).unwrap()
    );
}

#[test]
fn lazy_call_keys_share_cells_across_uses_without_forcing_arguments() {
    let p=program("scope unused = unused\npair :: Int -> (Int, Int)\npair x = (42, x)\nbad :: Int\nbad = error \"unforced\"\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    let owner = global(&s, "bad");
    let [a, b] = scoped_uses(&mut s, owner);
    let f = s.global_handle(&name("pair")).unwrap();
    let first = s.apply(f, &[handle(&s, a)]).unwrap();
    let first = s.force(first).unwrap();
    let hits = s.stats.application_cache_hits;
    let second = s.apply(f, &[handle(&s, b)]).unwrap();
    assert_eq!(s.force(second).unwrap(), first);
    assert!(s.stats.application_cache_hits > hits);
    assert_eq!(
        s.inspection_state(handle(&s, owner)).unwrap(),
        Inspection::Unevaluated
    );
    let child = s.children(first, 0, 1).unwrap()[0].handle;
    let value = s.force(child).unwrap();
    assert_eq!(
        s.view(value).unwrap(),
        ValueView::Literal(&Literal::Int(42))
    );
}

#[test]
fn shared_cells_memoize_errors_and_detect_black_holes() {
    for body in ["error \"expected\"", "answer"] {
        let p = program(&format!(
            "scope unused = unused\nanswer :: Int\nanswer = {body}\n"
        ));
        let mut s = Session::new(Arc::new(p), Limits::default());
        let owner = global(&s, "answer");
        let [a, b] = scoped_uses(&mut s, owner);
        let first = s.force(handle(&s, a)).unwrap_err();
        assert_eq!(s.force(handle(&s, b)).unwrap_err(), first);
        assert!(matches!(first, Outcome::Runtime(_)));
        if body == "answer" {
            assert!(matches!(first,Outcome::Runtime(e) if e.kind==RuntimeErrorKind::Loop));
        }
        assert_eq!(unused(&mut s, a), 10);
        assert_eq!(unused(&mut s, b), 20);
    }
}

#[test]
fn shared_returned_closures_keep_their_actual_captures() {
    let p=program("scope unused = unused\nmake :: Int -> Int -> Int\nmake x y = x + y\nanswer = make 7\narg :: Int\narg = 3\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    let owner = global(&s, "answer");
    let [a, b] = scoped_uses(&mut s, owner);
    for use_id in [a, b] {
        let arg = s.global_handle(&name("arg")).unwrap();
        let result = s.apply(handle(&s, use_id), &[arg]).unwrap();
        let result = s.force(result).unwrap();
        assert_eq!(
            s.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(10))
        );
    }
    assert_eq!(unused(&mut s, a), 10);
    assert_eq!(unused(&mut s, b), 20);
}

#[test]
fn graph_transfer_keeps_distinct_uses_of_one_cell_and_tracing_bypasses_sharing() {
    // A valid Core DAG with a shared expression site in two tuple positions.
    let mut p=program("cost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\nanswer :: Int\nanswer = cost 40\npair = (0, 0)\n");
    let answer = p.globals[&Global::Value(name("answer"))];
    let pair = p.globals[&Global::Value(name("pair"))];
    p.expressions[pair.0].kind = Kind::Tuple(vec![answer, answer]);
    let p = Arc::new(p);
    for tracked in [false, true] {
        let mut old = Session::new(p.clone(), Limits::default());
        if tracked {
            old.enable_dependency_tracking(Default::default()).unwrap();
        }
        old.evaluate(&name("answer")).unwrap();
        let value = old.evaluate(&name("pair")).unwrap();
        let uses = old.children(value, 0, 2).unwrap();
        assert_ne!(uses[0].handle, uses[1].handle);
        assert_eq!(
            old.cell_id(ThunkId(uses[0].handle.thunk))
                == old.cell_id(ThunkId(uses[1].handle.thunk)),
            !tracked
        );
        if tracked {
            continue;
        }
        let mut next = Session::new(p.clone(), Limits::default());
        assert!(next.reuse_from(&old).globals > 0);
        let foreign = uses[0].handle;
        drop(old);
        assert_eq!(next.force(foreign), Err(Outcome::ForeignValue));
        let value = next.evaluate(&name("pair")).unwrap();
        let uses = next.children(value, 0, 2).unwrap();
        assert_ne!(uses[0].handle, uses[1].handle);
        assert_eq!(
            next.cell_id(ThunkId(uses[0].handle.thunk)),
            next.cell_id(ThunkId(uses[1].handle.thunk))
        );
        let a = next.force(uses[0].handle).unwrap();
        assert_eq!(next.force(uses[1].handle).unwrap(), a);
        assert_eq!(next.view(a).unwrap(), ValueView::Literal(&Literal::Int(40)));
    }
}

#[cfg(feature = "incremental")]
fn argument_fixture(s: &mut Session, input: &str) -> (ExprId, EnvId) {
    let value = s.evaluate(&name("recipe")).unwrap();
    let Value::Closure(parameter, body, capture) = s.values[value.value].clone() else {
        panic!("recipe function")
    };
    let env = s.env(
        Some(capture.scope),
        Bindings::One(parameter, global(s, input)),
        Some(s.program.expressions[body.0].origin),
    );
    (body, env)
}

#[cfg(feature = "incremental")]
#[test]
fn recreated_lazy_arguments_keep_expensive_call_identity_after_forcing() {
    let p = program("recipe :: Int -> Int\nrecipe x = x + 1\ninput :: Int\ninput = 200\ncost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    s.limits.cache_minimum_work = 64;
    let (expr, env) = argument_fixture(&mut s, "input");
    let a = s.delay_expression(expr, env);
    let b = s.delay_expression(expr, env);
    let f = s.global_handle(&name("cost")).unwrap();
    let cold = s.apply(f, &[handle(&s, a)]).unwrap();
    let first = s.force(cold).unwrap();
    let before = s.stats.steps;
    let warm = s.apply(f, &[handle(&s, b)]).unwrap();
    let second = s.force(warm).unwrap();
    assert_eq!(first, second);
    assert!(matches!(s.cell_state(a), ThunkState::Done(Ok(_))));
    assert!(
        matches!(s.cell_state(b), ThunkState::Pending(_)),
        "lookup must not force the new argument"
    );
    let cost = s.stats.steps - before;
    eprintln!("recreated argument: cold={before}, warm={cost}");
    assert!(cost * 10 < before);
    assert_eq!(
        s.view(second).unwrap(),
        ValueView::Literal(&Literal::Int(201))
    );
}

#[cfg(feature = "incremental")]
#[test]
fn argument_recipe_identity_is_lazy_and_distinguishes_captures_and_unused_scope() {
    let p = program("scope unused = unused\nrecipe :: Int -> Int\nrecipe x = x + 1\na :: Int\na = 10\nb :: Int\nb = 20\nbad :: Int\nbad = error \"unforced\"\npair :: Int -> (Int, Int)\npair x = (42, x)\n");
    let mut s = Session::new(Arc::new(p), Limits::default());
    let (expr, env) = argument_fixture(&mut s, "bad");
    let a = s.delay_expression(expr, env);
    let b = s.delay_expression(expr, env);
    let f = s.global_handle(&name("pair")).unwrap();
    let first = s.apply(f, &[handle(&s, a)]).unwrap();
    let first = s.force(first).unwrap();
    let second = s.apply(f, &[handle(&s, b)]).unwrap();
    assert_eq!(s.force(second).unwrap(), first);
    for thunk in [a, b, global(&s, "bad")] {
        assert!(matches!(s.cell_state(thunk), ThunkState::Pending(_)));
    }
    let child = s.children(first, 0, 1).unwrap()[0].handle;
    let child = s.force(child).unwrap();
    assert_eq!(
        s.view(child).unwrap(),
        ValueView::Literal(&Literal::Int(42))
    );

    let (_, env_a) = argument_fixture(&mut s, "a");
    let (_, env_b) = argument_fixture(&mut s, "b");
    let a = s.delay_expression(expr, env_a);
    let b = s.delay_expression(expr, env_b);
    assert_ne!(
        s.argument_identity(s.cell_id(a)),
        s.argument_identity(s.cell_id(b))
    );

    let unused = *s
        .program
        .local_names
        .iter()
        .find(|(_, n)| *n == "unused")
        .unwrap()
        .0;
    let nested = s.env(Some(env_a), Bindings::One(unused, global(&s, "b")), None);
    let c = s.delay_expression(expr, nested);
    assert_eq!(
        s.argument_identity(s.cell_id(a)),
        s.argument_identity(s.cell_id(c))
    );
    assert!(!s.argument_scope_compatible(s.cell_id(a), s.cell_id(c)));
    let first = s.apply(f, &[handle(&s, a)]).unwrap();
    let first = s.force(first).unwrap();
    let second = s.apply(f, &[handle(&s, c)]).unwrap();
    assert_ne!(
        s.force(second).unwrap(),
        first,
        "same semantic inputs cannot hide an unused lexical binding"
    );
}

#[cfg(feature = "incremental")]
#[test]
fn argument_recipe_support_transfers_without_the_old_caller_result() {
    let common = "recipe :: Int -> Int\nrecipe x = x + 1\ninput :: Int\ninput = 200\ncost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\n";
    let mut old = Session::new(Arc::new(program(common)), Limits::default());
    old.limits.cache_minimum_work = 64;
    let (expr, env) = argument_fixture(&mut old, "input");
    let a = old.delay_expression(expr, env);
    let f = old.global_handle(&name("cost")).unwrap();
    let result = old.apply(f, &[handle(&old, a)]).unwrap();
    old.force(result).unwrap();
    let p = Arc::new(program(&format!("identity x = x\n{common}")));
    let mut next = Session::new(p.clone(), Limits::default());
    next.limits.cache_minimum_work = 64;
    let reused = next.reuse_from(&old);
    let (expr, env) = argument_fixture(&mut next, "input");
    let a = next.delay_expression(expr, env);
    let f = next.global_handle(&name("cost")).unwrap();
    let result = next.apply(f, &[handle(&next, a)]).unwrap();
    let result = next.force(result).unwrap();
    assert_eq!(
        next.view(result).unwrap(),
        ValueView::Literal(&Literal::Int(201))
    );
    let mut fresh = Session::new(p, Limits::default());
    let (expr, env) = argument_fixture(&mut fresh, "input");
    let a = fresh.delay_expression(expr, env);
    let f = fresh.global_handle(&name("cost")).unwrap();
    let result = fresh.apply(f, &[handle(&fresh, a)]).unwrap();
    fresh.force(result).unwrap();
    eprintln!(
        "argument transfer: reused={}, fresh={}, calls={}",
        next.stats.steps, fresh.stats.steps, reused.applications
    );
    assert!(reused.applications > 0);
    assert!(next.stats.steps * 10 < fresh.stats.steps);
}
