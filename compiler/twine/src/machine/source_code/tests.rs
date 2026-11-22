use super::*;
use knot_checker::check::{check_library_program, check_program};
fn name() -> GlobalName {
    GlobalName {
        module: vec!["Test".into()],
        name: "answer".into(),
    }
}
fn program() -> Arc<Program> {
    let raw =
        knot_syntax::parse("module Test exposing (..)\nanswer :: Int\nanswer = 20 + 22\n").unwrap();
    let canonical = knot_canonical::canonicalize_module(&raw, None).unwrap();
    Arc::new(knot_core::lower(&[
        check_library_program("Prelude").unwrap(),
        check_program(&canonical).unwrap(),
    ]))
}
fn fork(old: &Session, target: Arc<Program>, bound: SourceForkLimits) -> Session {
    old.try_fork_source(
        target,
        Arc::new(HostRegistry::default()),
        Limits::default(),
        bound,
    )
    .unwrap()
    .session
    .unwrap()
}
#[test]
fn identical_arena_and_origins_adopt_only_the_receiving_checked_program() {
    let code = program();
    let mut old = Session::new(code.clone(), Limits::default());
    let value = old.evaluate(&name()).unwrap();
    let target = Arc::new(code.as_ref().clone());
    let mut next = fork(&old, target.clone(), SourceForkLimits::default());
    assert!(next.source_code.is_none());
    assert!(Arc::ptr_eq(&next.program, &target));
    assert_eq!(next.view(value), Err(Outcome::ForeignValue));
    let evaluations = next.stats.thunk_evaluations;
    let result = next.evaluate(&name()).unwrap();
    assert_eq!(
        next.view(result).unwrap(),
        ValueView::Literal(&Literal::Int(42))
    );
    assert_eq!(next.stats.thunk_evaluations, evaluations);
}
#[test]
fn changed_origins_or_incomplete_proof_keep_relocation_and_source_visibility() {
    let code = program();
    let mut old = Session::new(code.clone(), Limits::default());
    old.evaluate(&name()).unwrap();
    let mut changed = code.as_ref().clone();
    let id = *changed.globals.get(&Global::Value(name())).unwrap();
    let original = changed.expressions[id.0].origin;
    let mut changed_origin = original;
    changed_origin.span.start += 1;
    changed_origin.span.end += 1;
    for index in 0..changed.expressions.len() {
        if changed.expressions[index].origin == original {
            changed.expressions[index].origin = changed_origin;
        }
    }
    let next = fork(&old, Arc::new(changed), SourceForkLimits::default());
    assert!(next.source_code.is_some());
    assert_eq!(next.current_source_origin(original), Some(changed_origin));
    let attempt = old
        .try_fork_source(
            code,
            Arc::new(HostRegistry::default()),
            Limits::default(),
            SourceForkLimits {
                code_nodes: 0,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(attempt
        .session
        .is_none_or(|next| next.source_code.is_some()));
}
