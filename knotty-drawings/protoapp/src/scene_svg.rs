//! SVG backend for the app's already-decoded Scene contract. This performs no
//! Knot evaluation and retains no runtime values. Knot's renderer remains the
//! reference/alternative backend. Attribute names and element kinds are fixed;
//! all source strings are escaped, including IDs, text and font families.
use serde_json::Value;
type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub fn render(scene: &Value) -> Result<String> {
    render_at_depth(scene, 0)
}
pub(super) fn render_at_depth(scene: &Value, depth: usize) -> Result<String> {
    let mut writer = Writer {
        output: String::new(),
        nodes: 0,
    };
    writer.node(scene, depth)?;
    Ok(writer.output)
}
fn field<'a>(object: &'a Value, name: &str) -> Result<&'a Value> {
    object
        .get(name)
        .ok_or_else(|| format!("scene SVG: missing {name}"))
}
fn text(value: &Value) -> Result<&str> {
    value
        .as_str()
        .ok_or_else(|| "scene SVG: expected text".into())
}
fn array(value: &Value) -> Result<&[Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "scene SVG: expected array".into())
}
fn number(value: &Value) -> Result<String> {
    value
        .as_number()
        .map(ToString::to_string)
        .ok_or_else(|| "scene SVG: expected finite number".into())
}
fn numbers(values: &[Value], separator: &str) -> Result<String> {
    Ok(values
        .iter()
        .map(number)
        .collect::<Result<Vec<_>>>()?
        .join(separator))
}
fn tuple(value: &Value, size: usize) -> Result<&[Value]> {
    let values = array(value)?;
    if values.len() != size {
        return Err("scene SVG: invalid coordinate tuple".into());
    }
    Ok(values)
}
struct Writer {
    output: String,
    nodes: usize,
}
impl Writer {
    fn push(&mut self, text: &str) -> Result<()> {
        if text.len() > MAX_BYTES.saturating_sub(self.output.len()) {
            return Err("scene SVG exceeds 64 MiB".into());
        }
        self.output.push_str(text);
        Ok(())
    }
    fn escaped(&mut self, value: &str) -> Result<()> {
        let mut start = 0;
        for (index, c) in value.char_indices() {
            let escaped = match c {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                _ => continue,
            };
            self.push(&value[start..index])?;
            self.push(escaped)?;
            start = index + c.len_utf8();
        }
        self.push(&value[start..])
    }
    fn attr(&mut self, name: &str, value: &str) -> Result<()> {
        self.push(" ")?;
        self.push(name)?;
        self.push("=\"")?;
        self.escaped(value)?;
        self.push("\"")
    }
    fn numeric(
        &mut self,
        props: &Value,
        name: &str,
        attr: &str,
        initial: Option<f64>,
    ) -> Result<()> {
        let value = field(props, name)?;
        if initial.is_some() && value.as_f64() == initial {
            return Ok(());
        }
        self.attr(attr, &number(value)?)
    }
    fn choice(&mut self, props: &Value, name: &str, attr: &str, choices: &[&str]) -> Result<()> {
        let value = text(field(props, name)?)?;
        if !choices.contains(&value) {
            return Err(format!("scene SVG: invalid {name}"));
        }
        if value != choices[0] {
            self.attr(attr, value)?;
        }
        Ok(())
    }
    fn paint(&mut self, props: &Value, name: &str) -> Result<Option<String>> {
        let value = field(props, name)?;
        if value.is_null() {
            return Ok(None);
        }
        let rgb = tuple(value, 3)?
            .iter()
            .map(|v| {
                v.as_i64()
                    .ok_or_else(|| "scene SVG: expected color channel".to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(format!("rgb({},{},{})", rgb[0], rgb[1], rgb[2])))
    }
    fn fill(&mut self, props: &Value) -> Result<()> {
        match self.paint(props, "fill.paint")? {
            None => self.attr("fill", "none"),
            Some(color) => {
                if color != "rgb(0,0,0)" {
                    self.attr("fill", &color)?;
                }
                self.numeric(props, "fill.opacity", "fill-opacity", Some(1.0))?;
                self.choice(props, "fill.rule", "fill-rule", &["nonzero", "evenodd"])
            }
        }
    }
    fn stroke(&mut self, props: &Value) -> Result<()> {
        let Some(color) = self.paint(props, "stroke.paint")? else {
            return Ok(());
        };
        self.attr("stroke", &color)?;
        self.numeric(props, "stroke.width", "stroke-width", Some(1.0))?;
        self.numeric(props, "stroke.opacity", "stroke-opacity", Some(1.0))?;
        self.choice(
            props,
            "stroke.cap",
            "stroke-linecap",
            &["butt", "round", "square"],
        )?;
        self.choice(
            props,
            "stroke.join",
            "stroke-linejoin",
            &["miter", "round", "bevel"],
        )?;
        self.numeric(props, "stroke.miterLimit", "stroke-miterlimit", Some(4.0))?;
        let dashes = array(field(props, "stroke.dashes")?)?;
        if !dashes.is_empty() {
            self.attr("stroke-dasharray", &numbers(dashes, " ")?)?;
        }
        self.numeric(props, "stroke.dashOffset", "stroke-dashoffset", Some(0.0))
    }
    fn common(&mut self, props: &Value) -> Result<()> {
        let id = field(props, "id")?;
        if !id.is_null() {
            self.attr("id", text(id)?)?;
        }
        let mut transform = Vec::new();
        for op in array(field(props, "transform")?)? {
            let op = array(op)?;
            let name = text(op.first().ok_or("scene SVG: empty transform")?)?;
            let valid = match name {
                "translate" | "scale" => op.len() == 3,
                "rotate" => op.len() == 2 || op.len() == 4,
                "skewX" | "skewY" => op.len() == 2,
                _ => false,
            };
            if !valid {
                return Err("scene SVG: invalid transform".into());
            }
            transform.push(format!("{}({})", name, numbers(&op[1..], ",")?));
        }
        if !transform.is_empty() {
            self.attr("transform", &transform.join(" "))?;
        }
        self.numeric(props, "opacity", "opacity", Some(1.0))?;
        match field(props, "visible")?.as_bool() {
            Some(true) => Ok(()),
            Some(false) => self.attr("display", "none"),
            None => Err("scene SVG: expected visibility boolean".into()),
        }
    }
    fn node(&mut self, node: &Value, depth: usize) -> Result<()> {
        self.nodes += 1;
        if depth > 256 || self.nodes > 1_000_000 {
            return Err("scene SVG exceeds node/depth limit".into());
        }
        let kind = text(field(node, "kind")?)?;
        let tag = match kind {
            "group" => "g",
            "svg" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path"
            | "text" => kind,
            _ => return Err("scene SVG: unknown element kind".into()),
        };
        if depth == 0 && kind != "svg" {
            return Err("scene SVG: expected document viewport".into());
        }
        let props = field(node, "props")?;
        self.push("<")?;
        self.push(tag)?;
        if depth == 0 {
            self.attr("xmlns", "http://www.w3.org/2000/svg")?;
        }
        let fields: &[&str] = match kind {
            "svg" => &["width", "height"],
            "rect" => &["x", "y", "width", "height"],
            "circle" => &["cx", "cy", "r"],
            "ellipse" => &["cx", "cy", "rx", "ry"],
            "line" => &["x1", "y1", "x2", "y2"],
            "text" => &["x", "y"],
            _ => &[],
        };
        for name in fields {
            self.numeric(props, name, name, None)?;
        }
        match kind {
            "svg" => {
                let view = field(props, "viewBox")?;
                if !view.is_null() {
                    self.attr("viewBox", &numbers(tuple(view, 4)?, " ")?)?;
                }
            }
            "rect" => {
                self.numeric(props, "rx", "rx", Some(0.0))?;
                self.numeric(props, "ry", "ry", Some(0.0))?;
            }
            "polyline" | "polygon" => {
                let points = array(field(props, "points")?)?
                    .iter()
                    .map(|p| numbers(tuple(p, 2)?, ","))
                    .collect::<Result<Vec<_>>>()?;
                self.attr("points", &points.join(" "))?;
            }
            "path" => {
                let mut path = Vec::new();
                for command in array(field(props, "commands")?)? {
                    let command = array(command)?;
                    let tag = text(command.first().ok_or("scene SVG: empty command")?)?;
                    let size = match tag {
                        "M" | "L" => 3,
                        "C" => 7,
                        "Z" => 1,
                        _ => 0,
                    };
                    if command.len() != size || size == 0 {
                        return Err("scene SVG: invalid path command".into());
                    }
                    path.push(if tag == "Z" {
                        tag.to_owned()
                    } else {
                        format!("{} {}", tag, numbers(&command[1..], " ")?)
                    });
                }
                self.attr("d", &path.join(" "))?;
            }
            "text" => {
                self.attr("font-family", text(field(props, "font.family")?)?)?;
                self.numeric(props, "font.size", "font-size", None)?;
                self.choice(
                    props,
                    "font.anchor",
                    "text-anchor",
                    &["start", "middle", "end"],
                )?;
                self.choice(
                    props,
                    "font.baseline",
                    "dominant-baseline",
                    &["alphabetic", "middle", "central", "hanging"],
                )?;
            }
            _ => {}
        }
        if !["group", "svg", "line"].contains(&kind) {
            self.fill(props)?;
        }
        if !["group", "svg"].contains(&kind) {
            self.stroke(props)?;
        }
        self.common(props)?;
        self.attr("data-key", text(field(node, "key")?)?)?;
        self.push(">")?;
        if kind == "text" {
            self.escaped(text(field(props, "content")?)?)?;
        }
        let children = array(field(node, "children")?)?;
        if !["svg", "group"].contains(&kind) && !children.is_empty() {
            return Err("scene SVG: shape has children".into());
        }
        for child in children {
            self.node(child, depth + 1)?;
        }
        self.push("</")?;
        self.push(tag)?;
        self.push(">")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_is_escaped_and_output_is_bounded() {
        let mut w = Writer {
            output: String::new(),
            nodes: 0,
        };
        w.attr("id", "\"<&>é").unwrap();
        assert_eq!(w.output, " id=\"&quot;&lt;&amp;&gt;é\"");
        w.output = "x".repeat(MAX_BYTES);
        assert!(w.push("x").is_err());
    }
    #[test]
    fn malformed_scene_is_rejected_without_arbitrary_tags_or_attributes() {
        assert!(render(&serde_json::json!({"kind":"script"})).is_err());
        assert!(render(&serde_json::json!({"kind":"svg","props":{}})).is_err());
        assert!(number(&Value::String("0\" onload=\"bad".into())).is_err());
    }
}
