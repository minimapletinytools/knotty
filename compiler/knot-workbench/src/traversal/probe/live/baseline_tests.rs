use super::*;
use knot_core::Literal;
use std::sync::atomic::{AtomicBool, AtomicUsize};

fn setup() -> (
    ProjectSnapshot,
    crate::traversal::SourceEditor,
    OperationSession,
) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "knot-baseline-{}-{}.knot",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let snapshot = ProjectSnapshot::load(&path, &[], &Default::default(),
        Some("module Main exposing (..)\nsum :: Int -> Int\nsum n = if n == 0 then 0 else n + sum (n - 1)\nanswer :: Int\nanswer = sum 100\nsmall :: Int\nsmall = 7\n".into()),
        &Default::default()).unwrap();
    let editor = crate::traversal::SourceEditor::new().unwrap();
    let modules = snapshot
        .graph
        .modules
        .iter()
        .map(|m| m.program.as_ref())
        .collect::<Vec<_>>();
    let prepared = snapshot
        .prepare_operations(
            editor.libraries.clone(),
            &Default::default(),
            &["answer", "small"],
        )
        .unwrap()
        .with_source_context(&modules, 1_000_000)
        .unwrap();
    let mut session = prepared.start(twine::Limits::default()).unwrap();
    let answer = session.binding("answer").unwrap();
    session.force(&answer).unwrap();
    (snapshot, editor, session)
}
fn evaluate(probes: &mut Probes, expression: &str) -> i64 {
    let (value, _) = probes.evaluate_lazy(expression, None).unwrap();
    let Data::Literal(Literal::Int(n)) = probes.decode_live(&value).unwrap() else {
        panic!("Int")
    };
    n
}
#[test]
fn sibling_queries_share_completed_values_but_not_requests_or_candidate_worlds() {
    let (snapshot, editor, session) = setup();
    let context = editor
        .context_reusing(&snapshot, &session, twine::Limits::default())
        .unwrap();
    assert!(context.baseline_stats().shared_heap_pages > 0);
    let baseline_steps = session.stats().steps;
    for n in [1, 2, 1] {
        let expression = format!("Main.answer + {n}");
        let mut reused = Probes::from_context(context.probes.clone(), EditLimits::default());
        let mut fresh = Probes::from_context(
            editor.context(&snapshot).unwrap().probes,
            EditLimits::default(),
        );
        assert_eq!(evaluate(&mut reused, &expression), 5050 + n);
        assert_eq!(evaluate(&mut fresh, &expression), 5050 + n);
        assert_eq!(reused.runtime_forks, 1);
        assert!(reused.runtime_fork_work > 0);
        assert_eq!(
            reused.usage.steps,
            reused.live.as_ref().unwrap().session.stats().steps
        );
        assert!(
            reused
                .live
                .as_ref()
                .unwrap()
                .session
                .stats()
                .thunk_evaluations
                < fresh
                    .live
                    .as_ref()
                    .unwrap()
                    .session
                    .stats()
                    .thunk_evaluations
        );
    }
    let candidate =
        ProbeContext::new_reusing(&snapshot, &editor.libraries, Some(&context.probes)).unwrap();
    assert!(candidate.baseline.lock().unwrap().is_none());
    assert_eq!(session.stats().steps, baseline_steps);
}
#[test]
fn bounded_miss_charges_work_and_preparation_cancellation_is_not_a_fresh_retry() {
    let (snapshot, editor, session) = setup();
    let context = editor
        .context_reusing(&snapshot, &session, twine::Limits::default())
        .unwrap();
    let limits = EditLimits {
        runtime: twine::Limits {
            heap_entries: session.heap_entries() - 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut probes = Probes::from_context(context.probes, limits);
    assert_eq!(evaluate(&mut probes, "Main.small"), 7);
    assert_eq!(probes.runtime_forks, 0);
    assert_eq!(
        probes.usage.steps,
        probes.runtime_fork_work + probes.live.as_ref().unwrap().session.stats().steps
    );
    let result = editor.context_reusing(
        &snapshot,
        &session,
        twine::Limits {
            cancellation: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(EditError::Runtime(twine::Outcome::Cancelled))
    ));
    let result = editor.context_reusing(
        &snapshot,
        &session,
        twine::Limits {
            steps: 1,
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(EditError::Runtime(twine::Outcome::BudgetExhausted))
    ));
}
