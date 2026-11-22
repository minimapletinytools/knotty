# Document transactions through the shared source editor

`SourceEditor::prepare_document` runs the same routing, policies, budgets and
candidate validation as the drawing app, returning a revision-bound document
edit. `FloatEditor` remains a small convenience wrapper that formats `Just target`
as a `Maybe Float` request. Its independent evaluator, literal selector and
single-call discovery code have been removed.

```rust,ignore
let editor = SourceEditor::new()?;
let mut document = Document::new(source);
let edit = editor.prepare_document(&document, "result", &Request {
    expression: "Just 10.0".into(),
    type_name: "Maybe Float".into(),
}, EditLimits::default())?;
// Review edit.patches(), edit.candidate_source(), and edit.usage().
let undo = edit.apply(&mut document)?;
undo.apply(&mut document)?;
```

`Document::at_path(source, path)` gives a buffer its module/import location. The
entry need not exist yet, but its parent must exist. Neither preparation, applying
an edit nor undo writes files. Imported modules are read-only. Apply/undo recheck
import bytes and resolution, including symlink retargeting, before changing the
buffer. Stale or foreign document revisions also reject. `EditHistory` supports
consecutive undo and retains its history when a dependency check fails.

The shared editor supports multiple patches and its documented source routes,
including computed inputs and selected branches. No-op requests preserve source
spelling and do not execute unravel rules. Selected failures never fall back to
later annotations; generic candidate policies run through the same checked path.
Nonfinite Float targets still reject before preparation.

The old `patch()`, `call()` and `rule()` accessors described a single Float call
and have been retired; use `patches()` for actual changes. Rule registration and
selection remain covered by the shared checker tests, rather than duplicated
Float-only metadata machinery. Replacement formatting follows SourceEditor
(e.g. a positive Float replacement may be parenthesized).

Run the original Float example, now using the shared executor:

```sh
cargo run --manifest-path compiler/Cargo.toml -p knot-workbench --example unravel_float
```

It requests `result = 10.0`, changes the original input from 3.0 to 5.0, then
undoes the edit without writing a file. See source traversal
for supported routes/limits, instance materialization for
custom policies, and integration for disk-backed applications.
