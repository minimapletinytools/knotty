use super::*;
use crate::{
    modules::EntryExtension,
    traversal::{Request, SourceEditor},
    unravel::EditLimits,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "knot-edit-domain-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn snapshot(&self, source: &str, extension: &EntryExtension) -> ProjectSnapshot {
        ProjectSnapshot::load(
            &self.0.join("Main.knot"),
            &[],
            &Default::default(),
            Some(source.into()),
            extension,
        )
        .unwrap()
    }
    fn changed(&self, original: &ProjectSnapshot, source: &str) -> ProjectSnapshot {
        self.snapshot(source, original.entry_extension())
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn patch(source: &str, before: &str, replacement: &str) -> LiteralPatch {
    let start = source.find(before).unwrap() as u32;
    LiteralPatch {
        span: Span::new(start, start + before.len() as u32),
        before: before.into(),
        replacement: replacement.into(),
    }
}
const SOURCE: &str = "module Main exposing (..)\nwidth :: Int\nwidth = 10\nheight :: Int\nheight = 20\nprogram = width * 2\n";

#[test]
fn narrowing_enforces_permissions_during_actual_unravel_and_binds_exact_transition() {
    let p = Project::new();
    let original = p.snapshot(SOURCE, &Default::default());
    let editor = SourceEditor::new().unwrap();
    let context = editor.context(&original).unwrap();
    let width = patch(SOURCE, "10", "12").span;
    let domain = context.edit_domain().restrict(&[width]).unwrap();
    assert!(domain.restrict(&[patch(SOURCE, "20", "22").span]).is_err());
    let context = context.with_edit_domain(domain).unwrap();
    let request = Request {
        expression: "Just 24".into(),
        type_name: "Maybe Int".into(),
    };
    let edit = editor
        .prepare_in(
            &original,
            &context,
            "program",
            &request,
            &[],
            EditLimits::default(),
        )
        .unwrap();
    assert!(edit
        .source_change()
        .certifies(&original, edit.candidate_snapshot()));
    assert_eq!(edit.source_change().changed_expressions(), &[width]);
    let same_text = p.changed(&original, edit.candidate_source());
    assert!(!edit.source_change().certifies(&original, &same_text));
    assert!(!edit
        .source_change()
        .certifies(edit.candidate_snapshot(), &original));
    assert!(matches!(
        editor.prepare_in(
            &original,
            &context,
            "height",
            &request,
            &[],
            EditLimits::default()
        ),
        Err(EditError::EditDomainViolation(_))
    ));
    assert!(matches!(
        editor
            .context(&same_text)
            .unwrap()
            .with_edit_domain(context.edit_domain().clone()),
        Err(EditError::StaleRevision)
    ));
}

#[test]
fn readonly_reference_can_materialize_but_dependency_or_extension_changes_reject() {
    let p = Project::new();
    std::fs::write(
        p.0.join("Paint.knot"),
        "module Paint exposing (..)\ntomato :: Int\ntomato = 10\n",
    )
    .unwrap();
    let source =
        "module Main exposing (..)\nimport Paint\nprogram :: Int\nprogram = Paint.tomato\n";
    let extension = EntryExtension {
        imports: vec![],
        suffix: "\nexported = program\n".into(),
    };
    let original = p.snapshot(source, &extension);
    let domain = EditDomain::entry(&original, 4096).unwrap();
    let patches = [patch(source, "Paint.tomato", "(let x = 12 in x)")];
    let changed = domain.apply(&original, &patches).unwrap();
    let candidate = p.changed(&original, &changed);
    assert!(domain.certify(&original, &candidate, &patches).is_ok());
    let generated = bodies(&original.graph.entry().program.canonical)
        .last()
        .unwrap()
        .span;
    assert!(domain.restrict(&[generated]).is_err());
    let changed_extension = p.snapshot(
        &changed,
        &EntryExtension {
            imports: vec![],
            suffix: "\nexported = program + 1\n".into(),
        },
    );
    assert!(matches!(
        domain.certify(&original, &changed_extension, &patches),
        Err(EditError::StaleRevision)
    ));
    std::fs::write(
        p.0.join("Paint.knot"),
        "module Paint exposing (..)\ntomato :: Int\ntomato = 11\n",
    )
    .unwrap();
    let changed_library = p.changed(&original, &changed);
    assert!(matches!(
        domain.certify(&original, &changed_library, &patches),
        Err(EditError::StaleRevision)
    ));
}

#[test]
fn byte_confinement_cannot_authorize_declaration_or_precedence_escape() {
    let p = Project::new();
    let original = p.snapshot(SOURCE, &Default::default());
    let domain = EditDomain::entry(&original, 4096).unwrap();
    let patches = [patch(SOURCE, "10", "11\ninjected = 100")];
    let candidate = p.changed(&original, &domain.apply(&original, &patches).unwrap());
    assert!(matches!(
        domain.certify(&original, &candidate, &patches),
        Err(EditError::EditDomainViolation(_))
    ));
    // Replacing the left operand with an unparenthesized sum changes the
    // surrounding multiplication's parse, despite changing only allowed bytes.
    let patches = [patch(SOURCE, "width * 2", "width + 1 * 2")];
    let candidate = p.changed(&original, &domain.apply(&original, &patches).unwrap());
    assert!(domain.certify(&original, &candidate, &patches).is_ok()); // whole expression is allowed
    let start = SOURCE.find("program = width").unwrap() + "program = ".len();
    let patches = [LiteralPatch {
        span: Span::new(start as u32, (start + 5) as u32),
        before: "width".into(),
        replacement: "width + 1".into(),
    }];
    let candidate = p.changed(&original, &domain.apply(&original, &patches).unwrap());
    assert!(matches!(
        domain.certify(&original, &candidate, &patches),
        Err(EditError::EditDomainViolation(_))
    ));
}

#[test]
fn utf8_stale_overlapping_and_out_of_expression_patches_never_get_authority() {
    let p = Project::new();
    let source = "module Main exposing (..)\nprogram = \"é\"\n";
    let original = p.snapshot(source, &Default::default());
    let domain = EditDomain::entry(&original, 4096).unwrap();
    let good = patch(source, "\"é\"", "\"🍅\"");
    let candidate = p.changed(
        &original,
        &domain
            .apply(&original, std::slice::from_ref(&good))
            .unwrap(),
    );
    assert!(domain
        .certify(&original, &candidate, std::slice::from_ref(&good))
        .is_ok());
    assert!(domain
        .apply(&original, &[good.clone(), good.clone()])
        .is_err());
    let mut stale = good.clone();
    stale.before = "wrong".into();
    assert!(matches!(
        domain.apply(&original, &[stale]),
        Err(EditError::StaleRevision)
    ));
    let mut partial = good.clone();
    partial.span.start += 2;
    assert!(domain.apply(&original, &[partial]).is_err());
    assert!(domain
        .restrict(&[patch(source, "Main", "Other").span])
        .is_err());
    assert!(matches!(
        EditDomain::entry(&original, 1),
        Err(EditError::SourceTooLarge)
    ));
    let tiny = EditDomain::entry(&original, source.len()).unwrap();
    assert!(matches!(
        tiny.apply(&original, &[good]),
        Err(EditError::SourceTooLarge)
    ));
    let manual = p.changed(&original, &source.replace("program", "other"));
    assert!(domain.certify(&original, &manual, &[]).is_err());
}
