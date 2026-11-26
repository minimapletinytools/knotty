//! App-specific adapter from the checked Types.Container boundary to Scene's
//! native JSON contract. No intermediate Knot Scene.Json graph or receipt cache.
use crate::render_profile::Layer;
use knot_core::Literal;
use serde_json::{json, Map, Value};
use std::time::Instant;
use twine::{ThunkHandle, ValueHandle, ValueReader, ValueView};

type Record = ValueHandle;
type Result<T> = std::result::Result<T, String>;

pub fn source_root_key(reader: ValueReader<'_>, root: ValueHandle) -> Result<String> {
    let mut decoder = Decoder {
        reader,
        work: 0,
        bytes: 0,
        layers: None,
    };
    let record = decoder.record(root)?;
    let kind = decoder.field(&record, "kind")?;
    let (name, fields) = decoder.constructor(kind, "Types")?;
    match (name.as_str(), fields.len()) {
        ("Group", 0) => Ok("0".into()),
        ("Viewport", 1) => Ok(String::new()),
        _ => Err("expected drawing ContainerKind".into()),
    }
}

pub fn decode(
    reader: ValueReader<'_>,
    root: ValueHandle,
    layers: Option<&mut Vec<Layer>>,
) -> Result<Value> {
    Decoder {
        reader,
        work: 0,
        bytes: 0,
        layers,
    }
    .container(root, "", 0, true)
}

/// Use the displayed ID (including null) to narrow the search. Only equality of
/// value handles in the source session establishes an object reference.
pub fn anchor_keys(
    reader: ValueReader<'_>,
    root: ValueHandle,
    target: ValueHandle,
    displayed: &Value,
) -> Result<Vec<String>> {
    let mut d = Decoder {
        reader,
        work: 0,
        bytes: 0,
        layers: None,
    };
    let mut keys = Vec::new();
    d.anchor(root, target, displayed, &mut keys)?;
    Ok(keys)
}

/// Every displayed occurrence of every node in a `List Node` anchor.
pub fn anchor_list_keys(
    reader: ValueReader<'_>,
    root: ValueHandle,
    mut list: ValueHandle,
    displayed: &Value,
) -> Result<Vec<String>> {
    let mut d = Decoder {
        reader,
        work: 0,
        bytes: 0,
        layers: None,
    };
    let mut keys = Vec::new();
    loop {
        let (ctor, fields) = d.constructor(list, "Prelude")?;
        let [head, tail] = fields.as_slice() else {
            break;
        };
        if ctor != ":" {
            return Err("expected a list of drawing Node anchors".into());
        }
        let node = d.force(*head)?;
        d.anchor(root, node, displayed, &mut keys)?;
        list = d.force(*tail)?;
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

struct AnchorCandidate<'a> {
    index: usize,
    key: &'a str,
    children: Vec<AnchorCandidate<'a>>,
}
impl<'a> AnchorCandidate<'a> {
    fn children(parent: &'a Value, id: &Value) -> Vec<Self> {
        parent["children"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, node)| {
                let children = Self::children(node, id);
                (node["id"] == *id || !children.is_empty()).then(|| Self {
                    index,
                    key: node["key"].as_str().unwrap(),
                    children,
                })
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
struct Mark {
    time: Instant,
    thunks: u64,
    primitives: u64,
}
impl Mark {
    fn read(reader: &ValueReader<'_>) -> Self {
        let stats = reader.stats();
        Self {
            time: Instant::now(),
            thunks: stats.thunk_evaluations,
            primitives: stats.primitive_calls,
        }
    }
}

struct Decoder<'a, 'p> {
    reader: ValueReader<'a>,
    work: usize,
    bytes: usize,
    layers: Option<&'p mut Vec<Layer>>,
}
impl Decoder<'_, '_> {
    // Conservatively bound adapter allocations as well as runtime demands.
    // Each demand/list cell costs one; each scene node reserves another 64
    // units for its fixed JSON structure. Variable arrays/strings charge below.
    fn charge(&mut self, amount: usize) -> Result<()> {
        self.work += amount;
        if self.work > 1_000_000 {
            return Err("drawing decode exceeds one million work units".into());
        }
        Ok(())
    }
    fn force(&mut self, handle: ThunkHandle) -> Result<ValueHandle> {
        self.charge(1)?;
        self.reader
            .force(handle)
            .map_err(|e| format!("drawing evaluation: {e:?}"))
    }
    fn fields(&self, value: ValueHandle, count: usize) -> Result<Vec<ThunkHandle>> {
        let fields = self
            .reader
            .indexed_fields(value)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("expected indexed drawing fields")?;
        if fields.len() != count {
            return Err("invalid drawing payload".into());
        }
        Ok(fields.iter().collect())
    }
    fn record(&self, value: ValueHandle) -> Result<Record> {
        let fields = self
            .reader
            .record_fields(value)
            .map_err(|e| format!("{e:?}"))?
            .ok_or("expected drawing record")?;
        // Types' records have at most eight fields. Reject an incompatible
        // library schema before allocating unbounded adapter metadata.
        if fields.len() > 16 {
            return Err("unexpected drawing record size".into());
        }
        Ok(value)
    }
    fn field(&mut self, record: &Record, name: &str) -> Result<ValueHandle> {
        let handle = self
            .reader
            .record_fields(*record)
            .map_err(|e| format!("{e:?}"))?
            .and_then(|fields| fields.get(name))
            .ok_or_else(|| format!("missing drawing field {name}"))?;
        self.force(handle)
    }
    fn constructor(&self, value: ValueHandle, module: &str) -> Result<(String, Vec<ThunkHandle>)> {
        match self.reader.view(value).map_err(|e| format!("{e:?}"))? {
            ValueView::Constructor(n, count) if n.module == [module] && count <= 6 => {
                Ok((n.name.clone(), self.fields(value, count)?))
            }
            _ => Err(format!("expected {module} constructor")),
        }
    }
    fn text(&mut self, value: ValueHandle) -> Result<Value> {
        let ValueView::Literal(Literal::String(text)) =
            self.reader.view(value).map_err(|e| format!("{e:?}"))?
        else {
            return Err("expected drawing String".into());
        };
        self.bytes += text.len();
        if self.bytes > 64 * 1024 * 1024 {
            return Err("drawing strings exceed 64 MiB".into());
        }
        Ok(Value::String(text.clone()))
    }
    fn number(&self, value: ValueHandle) -> Result<Value> {
        match self.reader.view(value).map_err(|e| format!("{e:?}"))? {
            ValueView::Literal(Literal::Float(n)) => serde_json::Number::from_f64(*n)
                .map(Value::Number)
                .ok_or_else(|| "non-finite drawing Float".into()),
            _ => Err("expected drawing Float".into()),
        }
    }
    fn boolean(&self, value: ValueHandle) -> Result<bool> {
        let (name, fields) = self.constructor(value, "Prelude")?;
        match (name.as_str(), fields.len()) {
            ("True", 0) => Ok(true),
            ("False", 0) => Ok(false),
            _ => Err("expected drawing Bool".into()),
        }
    }
    fn optional(
        &mut self,
        value: ValueHandle,
        f: impl FnOnce(&mut Self, ValueHandle) -> Result<Value>,
    ) -> Result<Value> {
        let (name, fields) = self.constructor(value, "Prelude")?;
        match (name.as_str(), fields.as_slice()) {
            ("Nothing", []) => Ok(Value::Null),
            ("Just", [handle]) => {
                let inner = self.force(*handle)?;
                f(self, inner)
            }
            _ => Err("expected drawing Maybe".into()),
        }
    }
    fn list(
        &mut self,
        value: ValueHandle,
        f: impl FnMut(&mut Self, ValueHandle, usize) -> Result<Value>,
    ) -> Result<Value> {
        self.list_from(value, None, f)
    }
    fn list_from(
        &mut self,
        mut value: ValueHandle,
        mut mark: Option<Mark>,
        mut f: impl FnMut(&mut Self, ValueHandle, usize) -> Result<Value>,
    ) -> Result<Value> {
        let mut out = Vec::new();
        loop {
            self.charge(1)?;
            let (name, fields) = self.constructor(value, "Prelude")?;
            match (name.as_str(), fields.as_slice()) {
                ("[]", []) => return Ok(Value::Array(out)),
                (":", [head, tail]) => {
                    let item = self.force(*head)?;
                    out.push(f(self, item, out.len())?);
                    if let Some(start) = mark {
                        let end = Mark::read(&self.reader);
                        let child = out.last().unwrap();
                        self.layers.as_deref_mut().unwrap().push(Layer {
                            kind: child["kind"].as_str().unwrap().into(),
                            children: child["children"].as_array().unwrap().len(),
                            ms: (end.time - start.time).as_secs_f64() * 1e3,
                            thunks: end.thunks - start.thunks,
                            primitives: end.primitives - start.primitives,
                        });
                        // Demanding the tail discovers the *next* child.
                        mark = Some(Mark::read(&self.reader));
                    }
                    value = self.force(*tail)?;
                }
                _ => return Err("expected drawing List".into()),
            }
        }
    }
    fn numbers(
        &mut self,
        record: &Record,
        names: &[&str],
        prefix: &str,
        props: &mut Map<String, Value>,
    ) -> Result<()> {
        for name in names {
            let value = self.field(record, name)?;
            props.insert(format!("{prefix}{name}"), self.number(value)?);
        }
        Ok(())
    }
    fn numeric_record(&mut self, value: ValueHandle, names: &[&str]) -> Result<Value> {
        let record = self.record(value)?;
        names
            .iter()
            .map(|name| {
                let value = self.field(&record, name)?;
                self.number(value)
            })
            .collect::<Result<Vec<_>>>()
            .map(Value::Array)
    }
    fn enumeration(&self, value: ValueHandle, choices: &[(&str, &str)]) -> Result<Value> {
        let (name, fields) = self.constructor(value, "Types")?;
        if fields.is_empty() {
            if let Some((_, text)) = choices.iter().find(|(key, _)| *key == name) {
                return Ok(json!(text));
            }
        }
        Err(format!("unexpected drawing enum {name}"))
    }
    fn paint(&mut self, value: ValueHandle) -> Result<Value> {
        let (name, fields) = self.constructor(value, "Types")?;
        match (name.as_str(), fields.as_slice()) {
            ("NoPaint", []) => Ok(Value::Null),
            ("Solid", [color]) => {
                let color = self.force(*color)?;
                let (name, fields) = self.constructor(color, "Types")?;
                if name != "Rgb" || fields.len() != 3 {
                    return Err("expected Rgb".into());
                }
                let mut rgb = Vec::new();
                for handle in fields {
                    let value = self.force(handle)?;
                    let ValueView::Literal(Literal::Int(n)) =
                        self.reader.view(value).map_err(|e| format!("{e:?}"))?
                    else {
                        return Err("expected color Int".into());
                    };
                    rgb.push(json!(n));
                }
                Ok(Value::Array(rgb))
            }
            _ => Err("expected drawing Paint".into()),
        }
    }
    fn command(&mut self, value: ValueHandle, transform: bool) -> Result<Value> {
        let (name, fields) = self.constructor(value, "Types")?;
        let (tag, count) = match (transform, name.as_str()) {
            (true, "Translate") => ("translate", 2),
            (true, "Scale") => ("scale", 2),
            (true, "Rotate") => ("rotate", 2),
            (true, "SkewX") => ("skewX", 1),
            (true, "SkewY") => ("skewY", 1),
            (false, "MoveTo") => ("M", 2),
            (false, "LineTo") => ("L", 2),
            (false, "CurveTo") => ("C", 6),
            (false, "ClosePath") => ("Z", 0),
            _ => return Err("unexpected drawing command".into()),
        };
        if fields.len() != count {
            return Err("invalid drawing command arity".into());
        }
        let mut out = vec![json!(tag)];
        for (index, handle) in fields.into_iter().enumerate() {
            let value = self.force(handle)?;
            if tag == "rotate" && index == 1 {
                let center = self.optional(value, |d, v| {
                    if !matches!(
                        d.reader.view(v).map_err(|e| format!("{e:?}"))?,
                        ValueView::Tuple(2)
                    ) {
                        return Err("expected rotation center pair".into());
                    }
                    let mut center = Vec::new();
                    for handle in d.fields(v, 2)? {
                        let v = d.force(handle)?;
                        center.push(d.number(v)?);
                    }
                    Ok(Value::Array(center))
                })?;
                if let Value::Array(center) = center {
                    out.extend(center);
                }
            } else {
                out.push(self.number(value)?);
            }
        }
        Ok(Value::Array(out))
    }
    fn common(&mut self, value: ValueHandle, props: &mut Map<String, Value>) -> Result<()> {
        let r = self.record(value)?;
        let id = self.field(&r, "id")?;
        props.insert("id".into(), self.optional(id, Self::text)?);
        let transforms = self.field(&r, "transform")?;
        props.insert(
            "transform".into(),
            self.list(transforms, |d, v, _| d.command(v, true))?,
        );
        self.numbers(&r, &["opacity"], "", props)?;
        let visible = self.field(&r, "visible")?;
        props.insert("visible".into(), json!(self.boolean(visible)?));
        Ok(())
    }
    fn style(
        &mut self,
        value: ValueHandle,
        stroke: bool,
        props: &mut Map<String, Value>,
    ) -> Result<()> {
        let r = self.record(value)?;
        let prefix = if stroke { "stroke." } else { "fill." };
        let paint = self.field(&r, "paint")?;
        props.insert(format!("{prefix}paint"), self.paint(paint)?);
        self.numbers(
            &r,
            if stroke {
                &["width", "opacity", "miterLimit", "dashOffset"]
            } else {
                &["opacity"]
            },
            prefix,
            props,
        )?;
        let enums: &[(&str, &[(&str, &str)])] = if stroke {
            &[
                (
                    "cap",
                    &[
                        ("Butt", "butt"),
                        ("RoundCap", "round"),
                        ("Square", "square"),
                    ],
                ),
                (
                    "join",
                    &[
                        ("Miter", "miter"),
                        ("RoundJoin", "round"),
                        ("Bevel", "bevel"),
                    ],
                ),
            ]
        } else {
            &[("rule", &[("NonZero", "nonzero"), ("EvenOdd", "evenodd")])]
        };
        for (field, choices) in enums {
            let value = self.field(&r, field)?;
            props.insert(
                format!("{prefix}{field}"),
                self.enumeration(value, choices)?,
            );
        }
        if stroke {
            let dashes = self.field(&r, "dashes")?;
            props.insert(
                "stroke.dashes".into(),
                self.list(dashes, |d, v, _| d.number(v))?,
            );
        }
        Ok(())
    }
    fn shape(&mut self, value: ValueHandle, props: &mut Map<String, Value>) -> Result<String> {
        let (name, fields) = self.constructor(value, "Types")?;
        let [handle] = fields.as_slice() else {
            return Err("expected drawing Shape".into());
        };
        let value = self.force(*handle)?;
        let r = self.record(value)?;
        let (mut kind, numbers): (&str, &[&str]) = match name.as_str() {
            "Rect" => ("rect", &["x", "y", "width", "height", "rx", "ry"]),
            "Circle" => ("circle", &["cx", "cy", "r"]),
            "Ellipse" => ("ellipse", &["cx", "cy", "rx", "ry"]),
            "Line" => ("line", &["x1", "y1", "x2", "y2"]),
            "Polyline" => ("polyline", &[]),
            "Path" => ("path", &[]),
            "Text" => ("text", &["x", "y"]),
            _ => return Err("unexpected drawing Shape".into()),
        };
        self.numbers(&r, numbers, "", props)?;
        match name.as_str() {
            "Polyline" => {
                let closed = self.field(&r, "closed")?;
                let closed = self.boolean(closed)?;
                if closed {
                    kind = "polygon";
                }
                props.insert("closed".into(), json!(closed));
                let points = self.field(&r, "points")?;
                props.insert(
                    "points".into(),
                    self.list(points, |d, v, _| d.numeric_record(v, &["x", "y"]))?,
                );
            }
            "Path" => {
                let commands = self.field(&r, "commands")?;
                props.insert(
                    "commands".into(),
                    self.list(commands, |d, v, _| d.command(v, false))?,
                );
            }
            "Text" => {
                let content = self.field(&r, "content")?;
                props.insert("content".into(), self.text(content)?);
                let font = self.field(&r, "font")?;
                let font = self.record(font)?;
                let family = self.field(&font, "family")?;
                props.insert("font.family".into(), self.text(family)?);
                self.numbers(&font, &["size"], "font.", props)?;
                let anchor = self.field(&font, "anchor")?;
                props.insert(
                    "font.anchor".into(),
                    self.enumeration(
                        anchor,
                        &[
                            ("AnchorStart", "start"),
                            ("AnchorMiddle", "middle"),
                            ("AnchorEnd", "end"),
                        ],
                    )?,
                );
                let baseline = self.field(&font, "baseline")?;
                props.insert(
                    "font.baseline".into(),
                    self.enumeration(
                        baseline,
                        &[
                            ("BaselineAlphabetic", "alphabetic"),
                            ("BaselineMiddle", "middle"),
                            ("BaselineCentral", "central"),
                            ("BaselineHanging", "hanging"),
                        ],
                    )?,
                );
            }
            _ => {}
        }
        if name != "Line" {
            let fill = self.field(&r, "fill")?;
            self.style(fill, false, props)?;
        }
        let stroke = self.field(&r, "stroke")?;
        self.style(stroke, true, props)?;
        Ok(kind.into())
    }
    fn anchor(
        &mut self,
        root: ValueHandle,
        target: ValueHandle,
        displayed: &Value,
        keys: &mut Vec<String>,
    ) -> Result<()> {
        let (name, fields) = self.constructor(target, "Types")?;
        let common = match (name.as_str(), fields.as_slice()) {
            ("ShapeNode", [common, _]) => self.force(*common)?,
            ("ContainerNode", [container]) => {
                let container = self.force(*container)?;
                self.field(&container, "common")?
            }
            _ => return Err("expected a drawing Node anchor".into()),
        };
        let id = self.field(&common, "id")?;
        let id = self.optional(id, Decoder::text)?;
        let candidates = AnchorCandidate::children(displayed, &id);
        self.match_children(root, &candidates, target, keys)
    }

    fn match_children(
        &mut self,
        container: ValueHandle,
        candidates: &[AnchorCandidate<'_>],
        target: ValueHandle,
        keys: &mut Vec<String>,
    ) -> Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        let mut list = self.field(&container, "children")?;
        let mut index = 0;
        let mut candidates = candidates.iter().peekable();
        while let Some(candidate) = candidates.peek() {
            let (ctor, fields) = self.constructor(list, "Prelude")?;
            let [head, tail] = fields.as_slice() else {
                return Err("missing anchor path".into());
            };
            if ctor != ":" {
                return Err("expected drawing list".into());
            }
            if index == candidate.index {
                let node = self.force(*head)?;
                if node == target {
                    keys.push(candidate.key.to_owned());
                }
                if !candidate.children.is_empty() {
                    let (kind, args) = self.constructor(node, "Types")?;
                    let [child] = args.as_slice() else {
                        return Err("missing anchor container".into());
                    };
                    if kind != "ContainerNode" {
                        return Err("expected anchor container".into());
                    }
                    let child = self.force(*child)?;
                    self.match_children(child, &candidate.children, target, keys)?;
                }
                candidates.next();
                if candidates.peek().is_none() {
                    break;
                }
            }
            index += 1;
            list = self.force(*tail)?;
        }
        Ok(())
    }
    fn node(&mut self, value: ValueHandle, key: &str, depth: usize) -> Result<Value> {
        self.depth(depth)?;
        let (name, fields) = self.constructor(value, "Types")?;
        match (name.as_str(), fields.as_slice()) {
            ("ContainerNode", [container]) => {
                let v = self.force(*container)?;
                self.container(v, key, depth, false)
            }
            ("ShapeNode", [common, shape]) => {
                self.charge(64)?;
                let mut props = Map::new();
                let shape = self.force(*shape)?;
                let kind = self.shape(shape, &mut props)?;
                let common = self.force(*common)?;
                self.common(common, &mut props)?;
                Ok(Self::output(key, &kind, props, json!([])))
            }
            _ => Err("expected drawing Node".into()),
        }
    }
    fn depth(&self, depth: usize) -> Result<()> {
        if depth > 128 {
            Err("drawing nesting exceeds 128 containers".into())
        } else {
            Ok(())
        }
    }
    fn output(key: &str, kind: &str, props: Map<String, Value>, children: Value) -> Value {
        Value::Object(Map::from_iter([
            ("key".into(), Value::String(key.into())),
            ("kind".into(), Value::String(kind.into())),
            ("id".into(), props["id"].clone()),
            ("props".into(), Value::Object(props)),
            ("children".into(), children),
        ]))
    }
    fn container(
        &mut self,
        value: ValueHandle,
        key: &str,
        depth: usize,
        root: bool,
    ) -> Result<Value> {
        self.depth(depth)?;
        self.charge(64)?;
        let r = self.record(value)?;
        let kind = self.field(&r, "kind")?;
        let (name, fields) = self.constructor(kind, "Types")?;
        if root && key.is_empty() && name == "Group" && fields.is_empty() {
            let child = self.container(value, "0", depth + 1, true)?;
            // KnottyDrawings.as_document wraps group roots in the default viewport.
            let props = json!({"width":300.0,"height":150.0,"viewBox":null,"id":null,"transform":[],"opacity":1.0,"visible":true});
            return Ok(Self::output(
                "",
                "svg",
                props.as_object().unwrap().clone(),
                Value::Array(vec![child]),
            ));
        }
        let mut props = Map::new();
        let kind = match (name.as_str(), fields.as_slice()) {
            ("Group", []) => "group",
            ("Viewport", [handle]) => {
                let viewport = self.force(*handle)?;
                let viewport = self.record(viewport)?;
                self.numbers(&viewport, &["width", "height"], "", &mut props)?;
                let view_box = self.field(&viewport, "viewBox")?;
                props.insert(
                    "viewBox".into(),
                    self.optional(view_box, |d, v| {
                        d.numeric_record(v, &["minX", "minY", "width", "height"])
                    })?,
                );
                "svg"
            }
            _ => return Err("expected drawing ContainerKind".into()),
        };
        let common = self.field(&r, "common")?;
        self.common(common, &mut props)?;
        // A group used as the document root sits inside a synthetic SVG viewport.
        let mark = (root && self.layers.is_some()).then(|| Mark::read(&self.reader));
        let children = self.field(&r, "children")?;
        let children = self.list_from(children, mark, |d, v, i| {
            let key = if key.is_empty() {
                i.to_string()
            } else {
                format!("{key}.{i}")
            };
            d.node(v, &key, depth + 1)
        })?;
        Ok(Self::output(key, kind, props, children))
    }
}
