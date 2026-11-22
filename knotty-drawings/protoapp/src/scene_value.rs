//! Decode the checked Scene.Json boundary directly into native JSON. Numeric
//! values remain numeric; quoting/escaping happens once in serde at the HTTP edge.
use knot_core::Literal;
use serde_json::{Map, Value};
use twine::{ThunkHandle, ValueHandle, ValueReader, ValueView};
#[cfg(feature = "scene-observations")]
mod cache;
#[cfg(feature = "scene-observations")]
pub use cache::{decode_cached, DecodeCache, DecodeStats};

#[cfg(any(test, not(feature = "scene-observations")))]
pub fn decode(session: ValueReader<'_>, root: ValueHandle) -> Result<Value, String> {
    Decoder {
        session,
        nodes: 0,
        bytes: 0,
        #[cfg(feature = "scene-observations")]
        cache: None,
        #[cfg(feature = "scene-observations")]
        depths: Vec::new(),
    }
    .value(root, 0)
}
struct Decoder<'a> {
    session: ValueReader<'a>,
    nodes: usize,
    bytes: usize,
    #[cfg(feature = "scene-observations")]
    cache: Option<cache::Pass<'a>>,
    #[cfg(feature = "scene-observations")]
    depths: Vec<usize>,
}
impl Decoder<'_> {
    fn charge(&mut self) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > 1_000_000 {
            return Err("scene exceeds one million decoded values".into());
        }
        Ok(())
    }
    fn force(&mut self, handle: ThunkHandle) -> Result<ValueHandle, String> {
        // Export consumes a disposable render session. A failure discards the
        // whole candidate, so per-field inspection rollback is unnecessary.
        // Ordinary forcing preserves call caching and charges every field to
        // the same cumulative execution/heap/host/cancellation limits.
        self.session
            .force(handle)
            .map_err(|e| format!("scene evaluation: {e:?}"))
    }

    fn fields(&mut self, value: ValueHandle, count: usize) -> Result<Vec<ThunkHandle>, String> {
        let children = self
            .session
            .children(value, 0, count)
            .map_err(|e| format!("{e:?}"))?;
        if children.len() != count {
            return Err("invalid Scene.Json payload".into());
        }
        Ok(children.into_iter().map(|c| c.handle).collect())
    }
    fn text(&mut self, value: ValueHandle) -> Result<String, String> {
        let ValueView::Literal(Literal::String(text)) =
            self.session.view(value).map_err(|e| format!("{e:?}"))?
        else {
            return Err("expected scene String".into());
        };
        self.bytes += text.len();
        if self.bytes > 64 * 1024 * 1024 {
            return Err("scene strings exceed 64 MiB".into());
        }
        Ok(text.clone())
    }
    fn list(
        &mut self,
        mut value: ValueHandle,
        mut item: impl FnMut(&mut Self, ValueHandle) -> Result<(), String>,
    ) -> Result<(), String> {
        loop {
            self.charge()?;
            match self.session.view(value).map_err(|e| format!("{e:?}"))? {
                ValueView::Constructor(n, 0) if n.module == ["Prelude"] && n.name == "[]" => {
                    return Ok(())
                }
                ValueView::Constructor(n, 2) if n.module == ["Prelude"] && n.name == ":" => {}
                _ => return Err("expected scene List".into()),
            }
            let fields = self.fields(value, 2)?;
            let head = self.force(fields[0])?;
            item(self, head)?;
            value = self.force(fields[1])?;
        }
    }
    fn value(&mut self, value: ValueHandle, depth: usize) -> Result<Value, String> {
        let start = (self.nodes, self.bytes);
        #[cfg(feature = "scene-observations")]
        self.depths.push(depth);
        let result = self.value_uncached(value, depth)?;
        #[cfg(feature = "scene-observations")]
        {
            let deepest = self.depths.pop().unwrap();
            if let Some(parent) = self.depths.last_mut() {
                *parent = (*parent).max(deepest);
            }
            self.retain(value, &result, start, deepest - depth)?;
        }
        #[cfg(not(feature = "scene-observations"))]
        let _ = start;
        Ok(result)
    }
    fn value_uncached(&mut self, value: ValueHandle, depth: usize) -> Result<Value, String> {
        #[cfg(feature = "scene-observations")]
        if let Some(cached) = self.cached(value, depth)? {
            return Ok(cached);
        }
        self.charge()?;
        if depth > 256 {
            return Err("scene exceeds 256 nested JSON values".into());
        }
        let name = match self.session.view(value).map_err(|e| format!("{e:?}"))? {
            ValueView::Constructor(n, 0) if n.module == ["Scene"] && n.name == "JsonNull" => {
                return Ok(Value::Null)
            }
            ValueView::Constructor(n, 1) if n.module == ["Scene"] => n.name.clone(),
            _ => return Err("expected Scene.Json".into()),
        };
        let field = self.fields(value, 1)?[0];
        let payload = self.force(field)?;
        Ok(match name.as_str() {
            "JsonString" => Value::String(self.text(payload)?),
            "JsonNumber" => match self.session.view(payload).map_err(|e| format!("{e:?}"))? {
                ValueView::Literal(Literal::Float(n)) => {
                    Value::Number(serde_json::Number::from_f64(*n).ok_or("nonfinite scene number")?)
                }
                _ => return Err("expected scene Float".into()),
            },
            "JsonInt" => match self.session.view(payload).map_err(|e| format!("{e:?}"))? {
                ValueView::Literal(Literal::Int(n)) => Value::Number((*n).into()),
                _ => return Err("expected scene Int".into()),
            },
            "JsonBool" => match self.session.view(payload).map_err(|e| format!("{e:?}"))? {
                ValueView::Constructor(n, 0) if n.module == ["Prelude"] && n.name == "True" => {
                    Value::Bool(true)
                }
                ValueView::Constructor(n, 0) if n.module == ["Prelude"] && n.name == "False" => {
                    Value::Bool(false)
                }
                _ => return Err("expected scene Bool".into()),
            },
            "JsonArray" => {
                let mut items = Vec::new();
                self.list(payload, |decoder, item| {
                    items.push(decoder.value(item, depth + 1)?);
                    Ok(())
                })?;
                Value::Array(items)
            }
            "JsonObject" => {
                let mut fields = Map::new();
                self.list(payload, |decoder, item| {
                    if !matches!(
                        decoder.session.view(item).map_err(|e| format!("{e:?}"))?,
                        ValueView::Tuple(2)
                    ) {
                        return Err("expected scene object pair".into());
                    }
                    let pair = decoder.fields(item, 2)?;
                    let key = decoder.force(pair[0])?;
                    let key = decoder.text(key)?;
                    let value = decoder.force(pair[1])?;
                    fields.insert(key, decoder.value(value, depth + 1)?);
                    Ok(())
                })?;
                Value::Object(fields)
            }
            _ => return Err("unknown Scene.Json constructor".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot_core::GlobalName;
    use knot_workbench::{libraries::Libraries, modules::EntryExtension, project::ProjectSnapshot};
    use std::{path::Path, sync::Arc};
    use twine::Session;
    #[test]
    fn scene_fields_share_one_execution_budget() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR"));
        let snapshot = ProjectSnapshot::load(
            &app.join("tests/fixtures/demo.knot"), &[app.join("..")],
            &Default::default(),
            Some("module Main exposing (..)\nimport Scene\nexported = Scene.JsonArray (replicate 128 (Scene.JsonInt 1))\n".into()),
            &Default::default(),
        ).unwrap();
        let modules = snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .collect::<Vec<_>>();
        let program = Arc::new(
            knot_core::link::link_borrowed(
                Libraries::load(None).unwrap().0.core,
                &modules,
                &Default::default(),
            )
            .unwrap(),
        );
        let exported = GlobalName {
            module: vec!["Main".into()],
            name: "exported".into(),
        };
        for steps in [500, 100_000] {
            let mut session = Session::new(
                program.clone(),
                twine::Limits {
                    steps,
                    ..Default::default()
                },
            );
            let root = session.evaluate(&exported).unwrap();
            let result = decode(session.read_values(), root);
            if steps == 500 {
                assert!(result.unwrap_err().contains("BudgetExhausted"));
                assert_eq!(session.stats().steps, steps);
            } else {
                assert_eq!(result.unwrap(), serde_json::json!(vec![1; 128]));
                assert!(session.stats().steps > 500);
            }
        }
    }

    #[test]
    fn typed_scene_matches_text_export_for_all_fixture_shapes() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR"));
        for fixture in ["demo.knot", "properties.knot", "handles.knot"] {
            let extension = EntryExtension {
                imports: vec![(vec!["Scene".into()], "SceneExport".into())],
                suffix: "\nlegacyScene :: String\nlegacyScene = SceneExport.scene_json program\ntypedScene :: SceneExport.Json\ntypedScene = SceneExport.scene_value program\ncontrols :: SceneExport.Json\ncontrols = SceneExport.JsonString \"é\\t\\n\\u{1}\\\"\\\\\"\n".into(),
            };
            let snapshot = ProjectSnapshot::load(
                &app.join("tests/fixtures").join(fixture),
                &[app.join("..")],
                &Default::default(),
                None,
                &extension,
            )
            .unwrap();
            let modules: Vec<_> = snapshot
                .graph
                .modules
                .iter()
                .map(|m| m.program.as_ref())
                .collect();
            let core = knot_core::link::link_borrowed(
                Libraries::load(None).unwrap().0.core,
                &modules,
                &Default::default(),
            )
            .unwrap();
            let mut session = Session::new(Arc::new(core), twine::Limits::default());
            let name = |n: &str| GlobalName {
                module: snapshot.graph.entry().program.canonical.name.clone(),
                name: n.into(),
            };
            let h = session.evaluate(&name("legacyScene")).unwrap();
            let ValueView::Literal(Literal::String(text)) = session.view(h).unwrap() else {
                panic!("text export");
            };
            let expected: Value = serde_json::from_str(text).unwrap();
            let h = session.evaluate(&name("typedScene")).unwrap();
            assert_eq!(
                decode(session.read_values(), h).unwrap(),
                expected,
                "{fixture}"
            );
            let h = session.evaluate(&name("controls")).unwrap();
            assert_eq!(
                decode(session.read_values(), h).unwrap(),
                Value::String("é\t\n\u{1}\"\\".into())
            );
        }
    }
}
