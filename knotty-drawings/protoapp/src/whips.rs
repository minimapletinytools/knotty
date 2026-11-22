//! Drawing-wide and object-bound integer sliders attached to checked entry-module bindings.
use knot_canonical::ast::{CDecl, CExpr, Ref};
use knot_checker::program::TypedProgram;
use knot_workbench::operation::ValueType;
use std::sync::Arc;
use twine::{Session, ValueView};

#[derive(serde::Serialize)]
pub struct Slider {
    #[serde(skip)]
    pub binding: String,
    #[serde(skip)]
    anchor: Option<String>,
    /// None is drawing-wide. A bound control has one key per occurrence in this snapshot.
    pub keys: Option<Vec<String>>,
    pub label: String,
    pub min: i32,
    pub max: i32,
    pub value: Option<i64>,
}

#[derive(serde::Deserialize)]
pub struct WhipEdit {
    pub snapshot: String,
    pub index: usize,
    pub key: Option<String>,
    pub value: i32,
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
    let (mut control, mut anchor) = (e, None);
    if let CExpr::App(rest, inner) = e {
        if let CExpr::App(ctor, reference) = &rest.node {
            if drawing_ctor(&ctor.node, "OnNode") {
                let CExpr::Var(Ref::TopLevel(name)) = &reference.node else {
                    return None;
                };
                let ty = ValueType::of_binding(program, name).ok()?;
                if !matches!(ty, ValueType::Named(n, args) if n.module == ["Types"] && n.name == "Node" && args.is_empty())
                {
                    return None;
                }
                anchor = Some(name.clone());
                control = &inner.node;
            }
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
        binding: binding.into(),
        keys: anchor.as_ref().map(|_| Vec::new()),
        anchor,
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
                if ValueType::of_binding(program, &f.name)
                    == Ok(ValueType::Named(knot_core::prelude("Int"), vec![]))
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
                if let Ok(keys) = crate::drawing_value::anchor_keys(
                    session.read_values(),
                    root,
                    target,
                    displayed,
                ) {
                    whip.keys = Some(keys);
                }
            }
        }
        if let Ok(handle) = session.evaluate(&global(&whip.binding)) {
            if let Ok(ValueView::Literal(knot_core::Literal::Int(value))) =
                session.read_values().view(handle)
            {
                whip.value = Some(*value);
            }
        }
    }
}
