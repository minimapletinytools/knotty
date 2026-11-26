//! Drawing-wide and object-bound controls attached to checked entry-module bindings:
//! integer sliders, and scale handles that multiply a Float from the canvas.
use knot_canonical::ast::{CDecl, CExpr, Ref};
use knot_checker::program::TypedProgram;
use knot_workbench::operation::ValueType;
use std::sync::Arc;
use twine::{Session, ValueView};

#[derive(serde::Serialize)]
pub struct Slider {
    /// "slider" edits an Int in the panel; "scale" multiplies a Float from a canvas handle.
    pub kind: &'static str,
    #[serde(skip)]
    pub binding: String,
    #[serde(skip)]
    anchor: Option<String>,
    /// The anchor is a `List Node`: the control belongs to every node in it.
    #[serde(skip)]
    anchor_list: bool,
    /// None is drawing-wide. A bound control has one key per occurrence in this snapshot.
    pub keys: Option<Vec<String>>,
    pub label: String,
    pub min: i32,
    pub max: i32,
    pub value: Option<serde_json::Number>,
}

#[derive(serde::Deserialize)]
pub struct WhipEdit {
    pub snapshot: String,
    pub index: usize,
    pub key: Option<String>,
    pub value: f64,
}

fn bound(e: &CExpr) -> Option<i32> {
    match e {
        CExpr::IntLit(n) => i32::try_from(n).ok(),
        CExpr::Negate(arg) => match &arg.node {
            CExpr::IntLit(n) => i32::try_from(-n).ok(),
            _ => None,
        },
        _ => None,
    }
}

fn drawing_ctor(expr: &CExpr, expected: &str) -> bool {
    matches!(expr, CExpr::Ctor(Ref::Imported {module, name}) if module == &["KnottyDrawings"] && name == expected)
}

fn slider(e: &CExpr, binding: &str, program: &TypedProgram) -> Option<Slider> {
    let (mut control, mut anchor, mut anchor_list) = (e, None, false);
    if let CExpr::App(rest, inner) = e {
        if let CExpr::App(ctor, reference) = &rest.node {
            let many = drawing_ctor(&ctor.node, "OnNodes");
            if many || drawing_ctor(&ctor.node, "OnNode") {
                let CExpr::Var(Ref::TopLevel(name)) = &reference.node else {
                    return None;
                };
                let node = |ty: &ValueType| matches!(ty, ValueType::Named(n, args) if n.module == ["Types"] && n.name == "Node" && args.is_empty());
                let ty = ValueType::of_binding(program, name).ok()?;
                let fits = match &ty {
                    ValueType::Named(n, args) if many => {
                        *n == knot_core::prelude("List") && args.len() == 1 && node(&args[0])
                    }
                    ty => !many && node(ty),
                };
                if !fits {
                    return None;
                }
                anchor = Some(name.clone());
                anchor_list = many;
                control = &inner.node;
            }
        }
    }
    if let CExpr::App(ctor, label) = control {
        if drawing_ctor(&ctor.node, "ScaleHandle") {
            let CExpr::StringLit(label) = &label.node else {
                return None;
            };
            // A scale handle lives on the canvas, so it needs something to sit on.
            return anchor.is_some().then(|| Slider {
                kind: "scale",
                binding: binding.into(),
                keys: Some(Vec::new()),
                anchor,
                anchor_list,
                label: label.clone(),
                min: 0,
                max: 0,
                value: None,
            });
        }
    }
    let CExpr::App(rest, high) = control else {
        return None;
    };
    let CExpr::App(rest, low) = &rest.node else {
        return None;
    };
    let CExpr::App(ctor, label) = &rest.node else {
        return None;
    };
    if !drawing_ctor(&ctor.node, "IntSlider") {
        return None;
    }
    let CExpr::StringLit(label) = &label.node else {
        return None;
    };
    let (min, max) = (bound(&low.node)?, bound(&high.node)?);
    (min < max).then(|| Slider {
        kind: "slider",
        binding: binding.into(),
        keys: anchor.as_ref().map(|_| Vec::new()),
        anchor,
        anchor_list,
        label: label.clone(),
        min,
        max,
        value: None,
    })
}

pub fn declared(program: &TypedProgram) -> Vec<Slider> {
    let mut whips = Vec::new();
    for decl in &program.canonical.decls {
        let CDecl::Fn(f) = &decl.node else { continue };
        for a in f.annotations.iter().filter(|a| a.key == "whip") {
            if let Some(control) = slider(&a.value.node, &f.name, program) {
                let wanted = if control.kind == "scale" { "Float" } else { "Int" };
                if ValueType::of_binding(program, &f.name)
                    == Ok(ValueType::Named(knot_core::prelude(wanted), vec![]))
                {
                    whips.push(control);
                }
            }
        }
    }
    whips
}

pub fn read_values(
    whips: &mut [Slider],
    core: &Arc<knot_core::Program>,
    module: &[String],
    scene: &serde_json::Value,
    source_root_key: &str,
) {
    if whips.is_empty() {
        return;
    }
    // Shared bindings retain their identity through lazy evaluation. Avoid merging
    // independent constructor applications through an optional application cache.
    let mut session = Session::new(
        core.clone(),
        twine::Limits {
            application_cache_entries: 0,
            ..Default::default()
        },
    );
    let global = |name: &str| knot_core::GlobalName {
        module: module.to_vec(),
        name: name.into(),
    };
    for whip in whips {
        if let Some(anchor) = &whip.anchor {
            if let (Ok(target), Ok(root), Some(displayed)) = (
                session.evaluate(&global(anchor)),
                session.evaluate(&global("program")),
                crate::find_node(scene, source_root_key),
            ) {
                let find = if whip.anchor_list {
                    crate::drawing_value::anchor_list_keys
                } else {
                    crate::drawing_value::anchor_keys
                };
                if let Ok(keys) = find(session.read_values(), root, target, displayed) {
                    whip.keys = Some(keys);
                }
            }
        }
        if let Ok(handle) = session.evaluate(&global(&whip.binding)) {
            match session.read_values().view(handle) {
                Ok(ValueView::Literal(knot_core::Literal::Int(value))) if whip.kind == "slider" => {
                    whip.value = Some((*value).into());
                }
                Ok(ValueView::Literal(knot_core::Literal::Float(value))) if whip.kind == "scale" => {
                    whip.value = serde_json::Number::from_f64(*value);
                }
                _ => {}
            }
        }
    }
}
