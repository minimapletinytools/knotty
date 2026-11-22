//! Scene schema -> typed requests. User text is encoded as values, never code.
use super::*;

pub(super) fn supported(kind: &str, field: &str) -> bool {
    let geometry: &[&str] = match kind {
        "rect" => &["x", "y", "width", "height", "rx", "ry"],
        "circle" => &["cx", "cy", "r"],
        "ellipse" => &["cx", "cy", "rx", "ry"],
        "line" => &["x1", "y1", "x2", "y2"],
        "polyline" | "polygon" => &["points", "closed"],
        "path" => &["commands"],
        "text" => &[
            "x",
            "y",
            "content",
            "font.family",
            "font.size",
            "font.anchor",
            "font.baseline",
        ],
        "svg" => &["width", "height", "viewBox"],
        "group" => &[],
        _ => return false,
    };
    geometry.contains(&field)
        || ["id", "opacity", "visible", "transform"].contains(&field)
        || (!matches!(kind, "svg" | "group")
            && [
                "stroke.paint",
                "stroke.width",
                "stroke.opacity",
                "stroke.cap",
                "stroke.join",
                "stroke.miterLimit",
                "stroke.dashes",
                "stroke.dashOffset",
            ]
            .contains(&field))
        || (!matches!(kind, "svg" | "group" | "line")
            && ["fill.paint", "fill.opacity", "fill.rule"].contains(&field))
}

pub(super) fn property_request(
    key: &[usize],
    kind: &str,
    field: &str,
    value: FieldValue,
) -> Result<Request, String> {
    properties_request(
        key,
        kind,
        &[(field.to_owned(), value)].into_iter().collect(),
    )
}

pub(super) fn properties_request(
    key: &[usize],
    kind: &str,
    values: &std::collections::BTreeMap<String, FieldValue>,
) -> Result<Request, String> {
    request_in_frame(key, kind, values, &[])
}

pub(super) fn request_in_frame(
    key: &[usize],
    kind: &str,
    values: &std::collections::BTreeMap<String, FieldValue>,
    frames: &[Value],
) -> Result<Request, String> {
    if values.is_empty() || values.keys().any(|field| !supported(kind, field)) {
        return Err("This shape has no such editable property.".into());
    }
    if key.len() > 32 || key.iter().any(|&n| n > 1000) {
        return Err("Scene key exceeds bounded edit traversal.".into());
    }
    let mut common = vec![];
    let mut geometry = vec![];
    let mut nested = std::collections::BTreeMap::<&str, Vec<String>>::new();
    for (field, value) in values {
        let assignment = |name: &str| format!("{name} = Just ({})", value.source());
        if ["id", "opacity", "visible", "transform"].contains(&field.as_str()) {
            common.push(assignment(field));
        } else if let Some((group, name)) = field.split_once('.') {
            nested.entry(group).or_default().push(assignment(name));
        } else {
            geometry.push(assignment(field));
        }
    }
    for (group, fields) in nested {
        let default = match group {
            "fill" => "Fill",
            "stroke" => "Stroke",
            "font" => "Font",
            _ => unreachable!(),
        };
        geometry.push(format!("{group} = ({})", updated(default, &fields)));
    }
    let common = updated("Common", &common);
    let mut container = format!("let c = Requests.anyContainer in {{ c | common = ({common}) }}");
    let mut node = if matches!(kind, "svg" | "group") {
        if !geometry.is_empty() {
            container = format!("let c = Requests.anyContainer in {{ c | common = ({common}), kind = Requests.ViewportKind ({}) }}", updated("Viewport", &geometry));
        }
        format!("Requests.ContainerNodeRequest ({container})")
    } else {
        let shape = match kind {
            "rect" => "Rect",
            "circle" => "Circle",
            "ellipse" => "Ellipse",
            "line" => "Line",
            "polyline" | "polygon" => "Polyline",
            "path" => "Path",
            "text" => "Text",
            _ => unreachable!(),
        };
        let geometry = if geometry.is_empty() {
            "Requests.AnyShape".into()
        } else {
            format!("Requests.Request{shape} ({})", updated(shape, &geometry))
        };
        format!("Requests.ShapeNodeRequest ({common}) ({geometry})")
    };
    if key.is_empty() && !matches!(kind, "svg" | "group") {
        return Err("The drawing root must be a container.".into());
    }
    for (depth, index) in key.iter().enumerate().rev() {
        let mut children = format!("Prepend ({node}) AnyRemainder");
        for _ in 0..*index {
            children = format!("Prepend Requests.AnyNode ({children})");
        }
        let mut frame_fields = vec![format!("children = {children}")];
        if let Some(frame) = frames.get(depth) {
            let transform = decode_field("transform", &frame["props"]["transform"])?;
            frame_fields.push(format!(
                "common = ({})",
                updated(
                    "Common",
                    &[format!("transform = Just ({})", transform.source())]
                )
            ));
            if frame["kind"] == "svg" {
                let mut viewport = vec![];
                for field in ["width", "height", "viewBox"] {
                    viewport.push(format!(
                        "{field} = Just ({})",
                        decode_field(field, &frame["props"][field])?.source()
                    ));
                }
                frame_fields.push(format!(
                    "kind = Requests.ViewportKind ({})",
                    updated("Viewport", &viewport)
                ));
            }
        }
        container = format!(
            "let c = Requests.anyContainer in {{ c | {} }}",
            frame_fields.join(", ")
        );
        node = format!("Requests.ContainerNodeRequest ({container})");
    }
    Ok(Request {
        expression: container,
        type_name: "Requests.ContainerRequest".into(),
    })
}

fn updated(default: &str, fields: &[String]) -> String {
    if fields.is_empty() {
        format!("Requests.any{default}")
    } else {
        format!(
            "let r = Requests.any{default} in {{ r | {} }}",
            fields.join(", ")
        )
    }
}

impl FieldValue {
    pub(super) fn source(&self) -> String {
        match self {
            Self::Float(n) => format!("({n:?})"),
            Self::Bool(b) => if *b { "True" } else { "False" }.into(),
            Self::String(s) => quoted(s),
            Self::OptionalString(None) => "Nothing".into(),
            Self::OptionalString(Some(s)) => format!("Just {}", quoted(s)),
            Self::Constructor(c) => format!("Types.{c}"),
            Self::Paint(None) => "Types.NoPaint".into(),
            Self::Paint(Some([r, g, b])) => format!("Types.Solid (Types.Rgb {r} {g} {b})"),
            Self::Structured(source) => source.clone(),
        }
    }
}
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// Structured editors send bounded JSON arrays, not SVG/Knot snippets.
pub(super) fn decode_structured(field: &str, value: &Value) -> Result<FieldValue, String> {
    let invalid = || format!("invalid {field}: expected the documented JSON array format");
    let number = |v: &Value| {
        v.as_f64()
            .filter(|n| n.is_finite())
            .map(|n| format!("({n:?})"))
            .ok_or_else(invalid)
    };
    if field == "viewBox" {
        if value.is_null() {
            return Ok(FieldValue::Structured("Nothing".into()));
        }
        let a = value
            .as_array()
            .filter(|a| a.len() == 4)
            .ok_or_else(invalid)?;
        let n = a.iter().map(number).collect::<Result<Vec<_>, _>>()?;
        return Ok(FieldValue::Structured(format!(
            "Just {{ minX = {}, minY = {}, width = {}, height = {} }}",
            n[0], n[1], n[2], n[3]
        )));
    }
    let items = value
        .as_array()
        .filter(|a| a.len() <= 128)
        .ok_or_else(invalid)?;
    let mut result = vec![];
    for item in items {
        if field == "stroke.dashes" {
            result.push(number(item)?);
            continue;
        }
        let a = item.as_array().ok_or_else(invalid)?;
        if field == "points" {
            if a.len() != 2 {
                return Err(invalid());
            }
            result.push(format!(
                "{{ x = {}, y = {} }}",
                number(&a[0])?,
                number(&a[1])?
            ));
            continue;
        }
        let name = a.first().and_then(Value::as_str).ok_or_else(invalid)?;
        let (constructor, arity) = match (field, name) {
            ("commands", "M") => ("MoveTo", 2),
            ("commands", "L") => ("LineTo", 2),
            ("commands", "C") => ("CurveTo", 6),
            ("commands", "Z") => ("ClosePath", 0),
            ("transform", "translate") => ("Translate", 2),
            ("transform", "scale") => ("Scale", 2),
            ("transform", "rotate") if a.len() == 2 => ("Rotate", 1),
            ("transform", "rotate") => ("Rotate", 3),
            ("transform", "skewX") => ("SkewX", 1),
            ("transform", "skewY") => ("SkewY", 1),
            _ => return Err(invalid()),
        };
        if a.len() != arity + 1 {
            return Err(invalid());
        }
        let args = a[1..].iter().map(number).collect::<Result<Vec<_>, _>>()?;
        result.push(if constructor == "Rotate" {
            if arity == 1 {
                format!("Types.Rotate {} Nothing", args[0])
            } else {
                format!("Types.Rotate {} (Just ({}, {}))", args[0], args[1], args[2])
            }
        } else {
            format!("Types.{constructor} {}", args.join(" "))
        });
    }
    Ok(FieldValue::Structured(format!(
        "[{}]",
        result
            .iter()
            .map(|s| format!("({s})"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}
