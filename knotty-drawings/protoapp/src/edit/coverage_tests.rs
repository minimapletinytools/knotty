use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture {
    dir: PathBuf,
    drawing: Drawing,
    editor: Editor,
    original: String,
}
impl Fixture {
    fn new(program: &str) -> Self {
        Self::source(format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nimport Types exposing (..)\nprogram = {program}\n"))
    }
    fn source(original: String) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "knot-property-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        std::fs::write(&path, &original).unwrap();
        let drawing = Drawing::new(
            path,
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            None,
        )
        .unwrap();
        assert!(drawing.error().is_none(), "{:?}", drawing.error());
        Self {
            dir,
            drawing,
            editor: Editor::new(false),
            original,
        }
    }
    fn snapshot(&self) -> String {
        self.drawing.last_good().unwrap().snapshot.id().into()
    }
    fn value(&self, key: &str, field: &str) -> Value {
        crate::find_node(&self.drawing.last_good().unwrap().scene, key).unwrap()["props"][field]
            .clone()
    }
    fn apply(&mut self, key: &str, field: &str, value: Value) -> Value {
        let request = EditRequest {
            snapshot: self.snapshot(),
            key: key.into(),
            field: field.into(),
            value,
        };
        self.editor.apply(&mut self.drawing, &request)
    }
    fn edit_and_undo(&mut self, key: &str, field: &str, value: Value) {
        let result = self.apply(key, field, value.clone());
        assert_eq!(result["ok"], true, "{key}.{field}={value}: {result}");
        assert_eq!(self.value(key, field), value, "{key}.{field}");
        let snapshot = self.snapshot();
        assert_eq!(self.editor.undo(&mut self.drawing, &snapshot)["ok"], true);
        assert_eq!(
            std::fs::read_to_string(self.drawing.path()).unwrap(),
            self.original
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn all_shape_geometry_and_text_round_trip_to_source() {
    let mut f=Fixture::new("svg 400 300 [rect 1 2 30 40, rounded_rect 1 2 30 40 5, ellipse 10 20 5 6, line 1 2 3 4, polyline [point 1 2, point 3 4], polygon [point 1 2, point 3 4], path [MoveTo 1 2, LineTo 3 4, ClosePath], text 1 2 \"hello\"] |> set_view_box 0 0 400 300");
    for (key, field, value) in [
        ("0", "width", json!(45.0)),
        ("1", "ry", json!(8.0)),
        ("2", "rx", json!(9.0)),
        ("3", "y2", json!(8.0)),
        ("4", "points", json!([[5.0, 6.0], [3.0, 4.0]])),
        ("5", "points", json!([[1.0, 2.0], [7.0, 8.0]])),
        (
            "6",
            "commands",
            json!([["M", 10.0, 20.0], ["L", 3.0, 4.0], ["Z"]]),
        ),
        ("7", "content", json!("quotes \" \\ é\nnew line")),
        ("7", "x", json!(12.0)),
        ("", "width", json!(450.0)),
        ("", "viewBox", json!([0.0, 0.0, 500.0, 300.0])),
    ] {
        f.edit_and_undo(key, field, value);
    }
}

#[test]
fn styles_enums_booleans_and_ids_round_trip() {
    let mut f=Fixture::new("svg 100 100 [text 10 20 \"text\" |> set_fill_paint (Solid tomato) |> set_fill_opacity 0.8 |> set_fill_rule NonZero |> set_stroke_paint (Solid blue) |> set_stroke_width 2 |> set_stroke_opacity 0.9 |> set_stroke_linecap Butt |> set_stroke_linejoin Miter |> set_stroke_miterlimit 4 |> set_stroke_dasharray [4,2] |> set_stroke_dashoffset 0 |> set_font_family \"serif\" |> set_font_size 18 |> set_text_anchor AnchorStart |> set_dominant_baseline BaselineAlphabetic |> set_optional_id (Just \"label\") |> set_opacity 1 |> set_visible True |> set_transform [Translate 1 2, Rotate 30 (Just (10,20))]]");
    for (field, value) in [
        ("fill.paint", Value::Null),
        ("fill.opacity", json!(0.5)),
        ("fill.rule", json!("evenodd")),
        ("stroke.paint", json!([12, 34, 56])),
        ("stroke.width", json!(3.0)),
        ("stroke.opacity", json!(0.6)),
        ("stroke.cap", json!("round")),
        ("stroke.join", json!("bevel")),
        ("stroke.miterLimit", json!(5.0)),
        ("stroke.dashes", json!([6.0, 3.0])),
        ("stroke.dashOffset", json!(2.0)),
        ("font.family", json!("monospace")),
        ("font.size", json!(24.0)),
        ("font.anchor", json!("middle")),
        ("font.baseline", json!("hanging")),
        ("id", Value::Null),
        ("opacity", json!(0.7)),
        ("visible", json!(false)),
        (
            "transform",
            json!([["translate", 10.0, 20.0], ["rotate", 45.0, 15.0, 25.0]]),
        ),
    ] {
        f.edit_and_undo("0", field, value);
    }
}

#[test]
fn nested_groups_and_shared_style_setters_keep_their_semantics() {
    let mut f=Fixture::new("svg 100 100 [group [rect 1 2 3 4, ellipse 10 20 5 6] |> set_fill tomato |> set_opacity 0.8, nested_svg 40 50 [line 1 2 3 4 |> set_stroke blue]]");
    f.edit_and_undo("0.0", "width", json!(8.0));
    f.edit_and_undo("0", "opacity", json!(0.4));
    f.edit_and_undo("1", "width", json!(60.0));
    f.edit_and_undo("1.0", "stroke.paint", json!([2, 3, 4]));
    assert_eq!(f.apply("0.0", "fill.paint", json!([1, 2, 3]))["ok"], true);
    assert_eq!(f.value("0.0", "fill.paint"), json!([1, 2, 3]));
    assert_eq!(f.value("0.1", "fill.paint"), json!([1, 2, 3]));
}

#[test]
fn mapped_shapes_edit_selected_inputs_and_preserve_callback_captures() {
    let mut f = Fixture::new(
        "svg 100 100 [group (map (\\w -> rect 1 2 (2 * w) 4 |> set_fill tomato) [10,20])]",
    );
    assert_eq!(f.apply("0.1", "width", json!(50.0))["ok"], true);
    assert_eq!(f.value("0.0", "width"), json!(20.0));
    assert_eq!(f.value("0.1", "width"), json!(50.0));
    let before = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert!(
        before.contains("[10,(25.0)]") || before.contains("[10, (25.0)]"),
        "{before}"
    );
    assert_eq!(f.apply("0.1", "fill.paint", json!([1, 2, 3]))["ok"], false);
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), before);
}

#[test]
fn fixed_defaults_fail_and_explicit_point_list_growth_round_trips() {
    let mut f =
        Fixture::new("svg 100 100 [rect 1 2 3 4, polyline [point 1 2] |> set_closed False]");
    f.edit_and_undo("1", "closed", json!(true));
    f.edit_and_undo("1", "points", json!([[1.0, 2.0], [3.0, 4.0]]));
    for (key, field, value) in [("0", "rx", json!(5)), ("0", "font.size", json!(20))] {
        assert_eq!(f.apply(key, field, value)["ok"], false);
        assert_eq!(
            std::fs::read_to_string(f.drawing.path()).unwrap(),
            f.original
        );
    }
}

#[test]
fn structured_data_is_bounded_and_not_executable_source() {
    for (field, value) in [
        ("points", json!("[point 1 2]")),
        ("commands", json!([["System.exec", 1, 2]])),
        ("transform", json!([["rotate", 30, 1]])),
        ("viewBox", json!([1, 2, 3])),
        ("stroke.dashes", json!(vec![1; 129])),
    ] {
        assert!(decode_field(field, &value).is_err(), "{field}");
    }
}

#[test]
fn optional_values_constructor_changes_and_transform_centers_remain_editable() {
    let mut f=Fixture::new("svg 100 100 [rect 1 2 3 4 |> set_fill_paint NoPaint |> set_optional_id Nothing |> add_transform (Rotate 30 (Just (10,20)))] |> set_optional_view_box (Just { minX = 0, minY = 0, width = 100, height = 100 })");
    f.edit_and_undo("", "viewBox", Value::Null);
    f.edit_and_undo("", "viewBox", json!([1.0, 2.0, 150.0, 120.0]));
    f.edit_and_undo("0", "id", json!("rectangle"));
    f.edit_and_undo("0", "transform", json!([["rotate", 45.0, 15.0, 25.0]]));
    assert_eq!(f.apply("0", "fill.paint", json!([1, 2, 3]))["ok"], true);
    assert_eq!(f.apply("0", "fill.paint", json!([4, 5, 6]))["ok"], true);
    assert_eq!(f.value("0", "fill.paint"), json!([4, 5, 6]));
}

#[test]
fn concat_map_forwards_non_circle_geometry() {
    let mut f =
        Fixture::new("svg 100 100 (concatMap (\\x -> [line x 2 3 4, ellipse x 20 5 6]) [10,30])");
    let result = f.apply("3", "cx", json!(40.0));
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(f.value("0", "x1"), json!(10.0));
    assert_eq!(f.value("1", "cx"), json!(10.0));
    assert_eq!(f.value("2", "x1"), json!(40.0));
    assert_eq!(f.value("3", "cx"), json!(40.0));
}

fn rect_probes(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> std::collections::BTreeMap<String, Value> {
    [
        (
            "moveX",
            json!({"x":x+1.0,"y":y,"width":width,"height":height}),
        ),
        (
            "moveY",
            json!({"x":x,"y":y+1.0,"width":width,"height":height}),
        ),
        (
            "sizeX",
            json!({"x":x,"y":y,"width":width*1.05,"height":height}),
        ),
        (
            "sizeY",
            json!({"x":x,"y":y,"width":width,"height":height*1.05}),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}
#[test]
fn geometry_capabilities_check_the_source_without_writing_or_creating_history() {
    let mut f = Fixture::new("svg 100 100 [rect 10 20 30 40, rect 1 1 1 1] ");
    let snapshot = f.snapshot();
    let result = f.editor.geometry_check(
        &mut f.drawing,
        GeometryCheck {
            snapshot: snapshot.clone(),
            key: "0".into(),
            probes: rect_probes(10.0, 20.0, 30.0, 40.0),
        },
    );
    assert_eq!(result["ok"], true, "{result}");
    for name in ["moveX", "moveY", "sizeX", "sizeY"] {
        assert_eq!(result["capabilities"][name]["editable"], true, "{result}");
    }
    assert_eq!(f.snapshot(), snapshot);
    assert!(f.editor.undo.is_empty());
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );

    let mut f = Fixture::new("svg 100 100 (map (\\w -> rect 10 20 (2 * w) 40) [15])");
    let snapshot = f.snapshot();
    let result = f.editor.geometry_check(
        &mut f.drawing,
        GeometryCheck {
            snapshot,
            key: "0".into(),
            probes: rect_probes(10.0, 20.0, 30.0, 40.0),
        },
    );
    assert_eq!(
        result["capabilities"]["sizeX"]["editable"], true,
        "{result}"
    );
    for name in ["moveX", "moveY", "sizeY"] {
        assert_eq!(result["capabilities"][name]["editable"], false, "{result}");
    }
}
#[test]
fn combined_geometry_previews_are_atomic_cancelable_and_one_undo_step() {
    let mut f = Fixture::new("svg 100 100 [rect 10 20 30 40]");
    let snapshot = f.snapshot();
    assert_eq!(
        f.editor.preview(
            &mut f.drawing,
            PreviewRequest::BeginGeometry {
                session: "drag".into(),
                snapshot,
                key: "0".into()
            }
        )["ok"],
        true
    );
    for value in [
        json!({"x":15,"y":25,"width":35,"height":45}),
        json!({"x":20,"y":30,"width":40,"height":50}),
    ] {
        let snapshot = f.snapshot();
        let result = f.editor.preview(
            &mut f.drawing,
            PreviewRequest::Update {
                session: "drag".into(),
                snapshot,
                value,
            },
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(f.editor.undo.len(), 1);
    }
    let source = std::fs::read_to_string(f.drawing.path()).unwrap();
    let snapshot = f.snapshot();
    let result = f.editor.preview(
        &mut f.drawing,
        PreviewRequest::Update {
            session: "drag".into(),
            snapshot,
            value: json!({"x":99,"y":30,"width":-1,"height":50}),
        },
    );
    assert_eq!(result["ok"], false);
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), source);
    let revision = f.drawing.revision();
    assert_eq!(
        f.editor.preview(
            &mut f.drawing,
            PreviewRequest::Accept {
                session: "drag".into()
            }
        )["ok"],
        true
    );
    assert_eq!(f.drawing.revision(), revision);
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    let snapshot = f.snapshot();
    assert_eq!(
        f.editor.preview(
            &mut f.drawing,
            PreviewRequest::BeginGeometry {
                session: "drag2".into(),
                snapshot,
                key: "0".into()
            }
        )["ok"],
        true
    );
    let snapshot = f.snapshot();
    assert_eq!(
        f.editor.preview(
            &mut f.drawing,
            PreviewRequest::Update {
                session: "drag2".into(),
                snapshot,
                value: json!({"x":40,"y":50,"width":30,"height":40})
            }
        )["ok"],
        true
    );
    assert_eq!(
        f.editor.preview(
            &mut f.drawing,
            PreviewRequest::Cancel {
                session: "drag2".into()
            }
        )["ok"],
        true
    );
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}
#[test]
fn drag_requests_hold_other_geometry_and_the_coordinate_transform_fixed() {
    let mut f =
        Fixture::new("let n = 10 in svg 100 100 [rect n n 30 40 |> set_transform [Translate n 0]]");
    let snapshot = f.snapshot();
    let result = f.editor.geometry_check(
        &mut f.drawing,
        GeometryCheck {
            snapshot,
            key: "0".into(),
            probes: rect_probes(10.0, 10.0, 30.0, 40.0),
        },
    );
    assert_eq!(
        result["capabilities"]["moveX"]["editable"], false,
        "{result}"
    );
    assert_eq!(
        result["capabilities"]["moveY"]["editable"], false,
        "{result}"
    );
    assert_eq!(
        result["capabilities"]["sizeX"]["editable"], true,
        "{result}"
    );
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn geometry_checks_preserve_ancestor_transforms_and_viewports() {
    for (program, key, x) in [
        (
            "let n = 10 in svg 100 100 [group [rect n 20 30 40] |> set_transform [Translate n 0]]",
            "0.0",
            10.0,
        ),
        ("let n = 100 in svg n 100 [rect n 20 30 40]", "0", 100.0),
    ] {
        let mut f = Fixture::new(program);
        let snapshot = f.snapshot();
        let result = f.editor.geometry_check(
            &mut f.drawing,
            GeometryCheck {
                snapshot,
                key: key.into(),
                probes: rect_probes(x, 20.0, 30.0, 40.0),
            },
        );
        assert_eq!(
            result["capabilities"]["moveX"]["editable"], false,
            "{result}"
        );
        assert_eq!(
            result["capabilities"]["moveY"]["editable"], true,
            "{result}"
        );
        assert_eq!(
            std::fs::read_to_string(f.drawing.path()).unwrap(),
            f.original
        );
    }
}

#[test]
fn circle_creation_is_opt_in_atomic_reeditable_and_undoable() {
    let mut f =
        Fixture::new("svg 200 200 (editableShapes (map (\\x -> circle x 20 5) [10,30]) [])");
    let request = |f: &Fixture| CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "".into(),
        cx: 40.0,
        cy: 60.0,
        r: 15.0,
    };
    let first = request(&f);
    let probe = f.editor.create_circle(&mut f.drawing, &first, true);
    assert_eq!(probe["ok"], true, "{probe}");
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    for expected_key in ["2", "3"] {
        let req = request(&f);
        let result = f.editor.create_circle(&mut f.drawing, &req, false);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["key"], expected_key);
        assert_eq!(f.value(expected_key, "r"), json!(15.0));
    }
    let created = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert!(
        created.contains("map (\\x -> circle x 20 5) [10,30]"),
        "{created}"
    );
    assert!(
        created.contains("Types.ShapeNode") && created.contains("cx = (40.0)"),
        "{created}"
    );
    assert_eq!(
        f.editor.create_circle(&mut f.drawing, &first, false)["ok"],
        false
    );
    let edit = f.apply("3", "r", json!(18.0));
    assert_eq!(edit["ok"], true, "{edit}");
    let edit = f.apply("0", "cx", json!(12.0));
    assert_eq!(edit["ok"], true, "{edit}");
    let edit = f.apply("3", "fill.paint", json!([200, 20, 60]));
    assert_eq!(edit["ok"], true, "{edit}");
    for _ in 0..5 {
        let id = f.snapshot();
        let result = f.editor.undo(&mut f.drawing, &id);
        assert_eq!(result["ok"], true, "{result}");
    }
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn circle_creation_rejects_unopted_destinations_and_invalid_values() {
    for program in [
        "svg 200 200 []",
        "svg 200 200 [circle 10 20 5]",
        "svg 200 200 (map (\\x -> circle x 20 5) [10,30])",
    ] {
        let mut f = Fixture::new(program);
        let request = CreateCircleRequest {
            snapshot: f.snapshot(),
            parent: "".into(),
            cx: 40.0,
            cy: 60.0,
            r: 15.0,
        };
        let result = f.editor.create_circle(&mut f.drawing, &request, false);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(
            std::fs::read_to_string(f.drawing.path()).unwrap(),
            f.original
        );
    }
    let mut f = Fixture::new("svg 200 200 (editableShapes [] [])");
    for radius in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let request = CreateCircleRequest {
            snapshot: f.snapshot(),
            parent: "".into(),
            cx: 40.0,
            cy: 60.0,
            r: radius,
        };
        assert_eq!(
            f.editor.create_circle(&mut f.drawing, &request, false)["ok"],
            false
        );
    }
}

#[test]
fn circle_creation_honors_nested_destinations_pins_and_external_changes() {
    let mut f = Fixture::new(
        "svg 200 200 [group (editableShapes [] []) |> set_transform [Translate 20 30, Scale 2 3]]",
    );
    let request = CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "0".into(),
        cx: 10.0,
        cy: 20.0,
        r: 5.0,
    };
    let result = f.editor.create_circle(&mut f.drawing, &request, false);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["key"], "0.0");
    assert_eq!(f.value("0.0", "cx"), json!(10.0));
    let id = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &id)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    let request = CreateCircleRequest {
        snapshot: f.snapshot(),
        ..request
    };
    let external = format!("{}\n-- external\n", f.original);
    std::fs::write(f.drawing.path(), &external).unwrap();
    assert_eq!(
        f.editor.create_circle(&mut f.drawing, &request, false)["ok"],
        false
    );
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), external);
    let mut f =
        Fixture::new("let additions = [] in svg 200 200 (editableShapes additions additions)");
    let request = CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "".into(),
        cx: 10.0,
        cy: 20.0,
        r: 5.0,
    };
    let result = f.editor.create_circle(&mut f.drawing, &request, false);
    assert_eq!(
        result["ok"], false,
        "shared inputs must retain their pins: {result}"
    );
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn circle_creation_on_the_full_example_preserves_generated_scene_and_undoes() {
    let mut f = Fixture::new("svg 100 100 []");
    f.original = include_str!("../../../example-drawing.knot").to_string();
    std::fs::write(f.drawing.path(), &f.original).unwrap();
    f.drawing.refresh();
    assert!(f.drawing.error().is_none(), "{:?}", f.drawing.error());
    let children = f.drawing.last_good().unwrap().scene["children"]
        .as_array()
        .unwrap()
        .clone();
    let request = CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "".into(),
        cx: 100.0,
        cy: 100.0,
        r: 8.0,
    };
    let result = f.editor.create_circle(&mut f.drawing, &request, false);
    assert_eq!(result["ok"], true, "{result}");
    let actual = f.drawing.last_good().unwrap().scene["children"]
        .as_array()
        .unwrap();
    assert_eq!(actual.len(), children.len() + 1);
    assert_eq!(&actual[..children.len()], children.as_slice());
    let id = f.snapshot();
    let result = f.editor.undo(&mut f.drawing, &id);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn mapped_creation_inserts_input_records_and_preserves_callbacks_and_existing_code() {
    let mut f = Fixture::source(include_str!("../../../examples/mapped-creation.knot").into());
    let request = |f: &Fixture| CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "".into(),
        cx: 270.0,
        cy: 200.0,
        r: 18.0,
    };
    let req = request(&f);
    let probe = f.editor.create_circle(&mut f.drawing, &req, true);
    assert_eq!(probe["ok"], true, "{probe}");
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    for key in ["2", "3"] {
        let req = request(&f);
        let result = f.editor.create_circle(&mut f.drawing, &req, false);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["key"], key);
        assert_eq!(f.value(key, "cx"), json!(270.0));
        assert_eq!(f.value(key, "r"), json!(18.0));
        assert_eq!(f.value(key, "fill.paint"), json!([0, 0, 0]));
    }
    let created = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert!(created.contains("40 + 40"), "{created}");
    assert!(
        created.contains("growMap defaultCircleInput circleFromInput dots"),
        "{created}"
    );
    assert!(created.contains("radius = (18.0)"), "{created}");
    assert!(
        !created.contains("circle (270"),
        "new shapes must remain input data: {created}"
    );
    for (key, field, value) in [
        ("3", "r", json!(25.0)),
        ("0", "cx", json!(85.0)),
        ("3", "fill.paint", json!([200, 20, 60])),
    ] {
        let result = f.apply(key, field, value.clone());
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(f.value(key, field), value);
    }
    for _ in 0..5 {
        let snapshot = f.snapshot();
        assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    }
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn mapped_creation_uses_the_actual_callback_and_retains_seed_branch_guards() {
    for (callback, x, succeeds) in [
        (
            "\\d -> circle (d.x * 2) d.y d.radius |> set_fill d.color",
            60.0,
            true,
        ),
        (
            "\\d -> if d.x >= 0 then circleFromInput d else circleFromInput d",
            -10.0,
            false,
        ),
    ] {
        let mut f = Fixture::new(&format!(
            "svg 200 200 (growMap defaultCircleInput ({callback}) [])"
        ));
        let req = CreateCircleRequest {
            snapshot: f.snapshot(),
            parent: "".into(),
            cx: x,
            cy: 40.0,
            r: 15.0,
        };
        let result = f.editor.create_circle(&mut f.drawing, &req, false);
        assert_eq!(result["ok"], succeeds, "{result}");
        if succeeds {
            assert_eq!(f.value("0", "cx"), json!(x));
            let source = std::fs::read_to_string(f.drawing.path()).unwrap();
            assert!(source.contains("x = (30.0)"), "{source}");
            assert!(source.contains(callback), "{source}");
        } else {
            assert_eq!(
                std::fs::read_to_string(f.drawing.path()).unwrap(),
                f.original
            );
        }
    }
}

#[test]
fn custom_mapped_input_schema_registers_its_materializer_entirely_in_knot() {
    let source = include_str!("../../../examples/mapped-custom-inputs.knot");
    let mut f = Fixture::source(source.into());
    let request = CreateCircleRequest {
        snapshot: f.snapshot(),
        parent: "".into(),
        cx: 60.0,
        cy: 40.0,
        r: 15.0,
    };
    let result = f.editor.create_circle(&mut f.drawing, &request, false);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(f.value("2", "cx"), json!(60.0));
    assert_eq!(f.value("2", "fill.paint"), json!([255, 99, 71]));
    let created = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert!(created.contains("x = (30.0)"), "{created}");
    assert!(created.contains("label = \"new dot\""), "{created}");
    assert!(
        created.contains("growMap (fresh ()) (\\d -> circle (d.x * 2)"),
        "{created}"
    );
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), source);
}

#[test]
fn example_fill_edit_rotates_the_shared_gradient() {
    let mut f = Fixture::source(include_str!("../../../example-drawing.knot").into());
    let mut expected = f.drawing.last_good().unwrap().scene.as_ref().clone();
    let children = expected["children"].as_array_mut().unwrap();
    let rotate = |node: &mut Value| {
        let props = &mut node["props"];
        let field = if props["fill.paint"].is_null() {
            "stroke.paint"
        } else {
            "fill.paint"
        };
        let rgb = &props[field];
        props[field] = json!([rgb[2], rgb[0], rgb[1]]);
    };
    for sparkle in children[3]["children"].as_array_mut().unwrap() {
        rotate(sparkle);
    }
    for shape in &mut children[8..72] {
        rotate(shape);
    }
    let target = children[9]["props"]["fill.paint"].clone();
    let result = f.apply("9", "fill.paint", target);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(f.drawing.last_good().unwrap().scene.as_ref(), &expected);

    let edited = std::fs::read_to_string(f.drawing.path()).unwrap();
    let changes: Vec<_> = f
        .original
        .lines()
        .zip(edited.lines())
        .filter(|(before, after)| before != after)
        .collect();
    assert_eq!(edited.lines().count(), f.original.lines().count());
    assert_eq!(changes.len(), 1);
    assert!(changes[0].0.starts_with("gradientTint = "));
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn drawing_whips_edit_the_annotated_head_and_share_undo_history() {
    let mut f = Fixture::source(
        r#"module Drawing exposing (..)
import KnottyDrawings as D exposing (..)

base :: Int
base = 8

@whip(D.IntSlider "Density" 1 32)
@whip(IntSlider "Fine density" 4 12)
decorations :: Int
decorations = base

unused :: Int
unused = 6

@whip(IntSlider "Wrong type" 1 8)
label :: String
label = "text"

program = svg 80 80 [circle (fromIntegral decorations) 10 4]
"#
        .into(),
    );
    let snap = f.snapshot();
    let controls = &f.drawing.last_good().unwrap().whips;
    assert_eq!(controls.len(), 2);
    assert_eq!(
        (&*controls[1].label, controls[1].value),
        ("Fine density", Some(8))
    );
    let mut edit = crate::whips::WhipEdit {
        snapshot: snap,
        key: None,
        index: 1,
        value: 18,
    };
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], false);
    edit.index = 2;
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], false);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    edit.index = 0;
    let result = f.editor.apply_whip(&mut f.drawing, &edit);
    assert_eq!(result["ok"], true, "{result}");
    let source = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert!(source.contains("decorations = base"));
    assert_eq!(f.value("0", "cx"), json!(18.0));
    assert_eq!(f.drawing.last_good().unwrap().whips[0].value, Some(18));
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], false);
    let snap = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snap)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
    assert_eq!(f.drawing.last_good().unwrap().whips[0].value, Some(8));
}

#[test]
fn object_whips_follow_references_across_occurrences_and_revisions() {
    let mut f = Fixture::source(
        r#"module Drawing exposing (..)
import KnottyDrawings exposing (..)

@whip(OnNode curve (IntSlider "Count" 1 16))
@whip(OnNode anonymous (IntSlider "Anonymous" 1 16))
@whip(OnNode absent (IntSlider "Absent" 1 16))
count :: Int
count = 8

curve = circle 1 2 3 |> set_id "shared-id"
other = circle 1 2 3 |> set_id "shared-id"
absent = circle 1 2 3 |> set_id "shared-id"
anonymous = circle 4 5 6
program = svg 80 80 [other, group [curve, anonymous], curve]
"#
        .into(),
    );
    let keys = |f: &Fixture, i: usize| {
        f.drawing.last_good().unwrap().whips[i]
            .keys
            .clone()
            .unwrap()
    };
    assert_eq!(keys(&f, 0), ["1.0", "2"]);
    assert_eq!(keys(&f, 1), ["1.1"]);
    assert!(keys(&f, 2).is_empty());
    let mut edit = crate::whips::WhipEdit {
        snapshot: f.snapshot(),
        index: 0,
        key: Some("0".into()),
        value: 10,
    };
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], false);
    edit.key = Some("1.0".into());
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], true);
    let source = f.original.replace(
        "[other, group [curve, anonymous], curve]",
        "[anonymous, other, curve]",
    );
    std::fs::write(f.drawing.path(), source).unwrap();
    f.drawing.refresh();
    assert_eq!(keys(&f, 0), ["2"]);
    assert_eq!(keys(&f, 1), ["0"]);
    assert_eq!(f.editor.apply_whip(&mut f.drawing, &edit)["ok"], false);
}

fn save_text(fixture: &mut Fixture, source: &str, coalesce: bool) -> Value {
    let edit = serde_json::from_value(json!({
        "snapshot": fixture.snapshot(), "source": source, "coalesce": coalesce,
    }))
    .unwrap();
    fixture.editor.save_text(&mut fixture.drawing, edit)
}

#[test]
fn source_text_publishes_valid_source_and_rejects_invalid_source_unwritten() {
    let mut f = Fixture::new("svg 200 100 [circle 20 30 12]");
    let grown = f.original.replace("circle 20 30 12", "circle 20 30 16");
    assert_eq!(save_text(&mut f, &grown, false)["ok"], true);
    assert_eq!(f.value("0", "r"), json!(16.0));
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), grown);
    for broken in [grown.replace("16]", "16"), grown.replace("16", "\"x\"")] {
        let result = save_text(&mut f, &broken, true);
        assert_eq!(result["ok"], false, "{broken}");
        assert!(result["reason"].as_str().unwrap().contains(':'), "{result}");
        assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), grown);
        assert_eq!(f.value("0", "r"), json!(16.0));
    }
    let stale = serde_json::from_value(json!({"snapshot": "stale", "source": f.original})).unwrap();
    assert_eq!(f.editor.save_text(&mut f.drawing, stale)["ok"], false);
}

#[test]
fn source_text_bursts_undo_together_and_other_edits_start_new_steps() {
    let mut f = Fixture::new("svg 200 100 [circle 20 30 12]");
    let at = |r: &str| {
        f.original
            .replace("circle 20 30 12", &format!("circle 20 30 {r}"))
    };
    let (one, two) = (at("1"), at("15"));
    assert_eq!(save_text(&mut f, &one, false)["ok"], true);
    assert_eq!(save_text(&mut f, &two, true)["ok"], true);
    assert_eq!(f.apply("0", "cx", json!(40.0))["ok"], true);
    let edited = std::fs::read_to_string(f.drawing.path()).unwrap();
    assert_eq!(
        save_text(&mut f, &edited.replace("15", "18"), true)["ok"],
        true
    );
    // The coalesce flag cannot merge across the property edit.
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), edited);
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), two);
    let snapshot = f.snapshot();
    assert_eq!(f.editor.undo(&mut f.drawing, &snapshot)["ok"], true);
    assert_eq!(
        std::fs::read_to_string(f.drawing.path()).unwrap(),
        f.original
    );
}

#[test]
fn locate_names_the_lines_behind_each_field_without_writing() {
    let mut f = Fixture::source("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nimport Types exposing (..)\npageWidth :: Float\npageWidth = 210\nprogram =\n    svg 300 300\n        [ rect 4 5 pageWidth 40 |> set_fill (rgb 250 247 240)\n        ]\n".into());
    let snapshot = f.snapshot();
    let found = f.editor.locate(&mut f.drawing, &snapshot, "0");
    assert_eq!(found["ok"], true, "{found}");
    let lines = found["lines"].as_array().unwrap();
    let at = |n: u64| lines.iter().find(|l| l["line"] == n).unwrap_or_else(|| panic!("line {n}: {found}"));
    assert_eq!(at(5)["fields"], json!(["width"]));
    assert_eq!(at(5)["text"], "pageWidth = 210");
    for field in ["x", "y", "height", "fill.paint"] {
        assert!(at(8)["fields"].as_array().unwrap().contains(&json!(field)), "{field}: {found}");
    }
    assert_eq!(std::fs::read_to_string(f.drawing.path()).unwrap(), f.original);
    assert!(!f.editor.can_undo(&f.drawing));
    assert_eq!(f.editor.locate(&mut f.drawing, "stale", "0")["ok"], false);
}
