//! Literal canvas coordinates are editor metadata. The parser has already
//! selected the winning key (including block precedence).
use knot_canonical::ast::{CAnnotation, CExpr};
use knot_syntax::Span;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Placement {
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    computed_position: bool,
    #[serde(skip)]
    pub span: Span,
    #[serde(skip)]
    pub before_binding: bool,
    #[serde(skip)]
    coordinates: Option<[Span; 2]>,
}

fn coordinate(expr: &CExpr) -> Option<f64> {
    let number = match expr {
        CExpr::IntLit(i) => i.to_string().parse().ok()?,
        CExpr::FloatLit(f) => f.to_f64(),
        CExpr::Negate(n) => -coordinate(&n.node)?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

impl Placement {
    pub fn new(span: Span) -> Self {
        Self {
            span,
            before_binding: false,
            position: None,
            coordinates: None,
            computed_position: false,
        }
    }

    pub fn read(&mut self, annotations: &[CAnnotation]) {
        let Some(entry) = annotations.iter().find(|a| a.key == "position") else {
            return;
        };
        let pair = match &entry.value.node {
            CExpr::Tuple(items) => match items.as_slice() {
                [x, y] => coordinate(&x.node)
                    .zip(coordinate(&y.node))
                    .map(|(a, b)| ([a, b], [x.span, y.span])),
                _ => None,
            },
            _ => None,
        };
        (self.position, self.coordinates) = pair.unzip();
        self.computed_position = pair.is_none();
    }

    pub fn edit(
        &self,
        xy: [f64; 2],
        definition: bool,
        atom: bool,
        source: &str,
        edits: &mut Vec<Patch>,
    ) -> Result<(), String> {
        if !xy.iter().all(|v| v.is_finite()) || self.computed_position {
            return Err("Moving a node requires a literal coordinate pair.".into());
        }
        if let (Some(previous), Some(spans)) = (self.position, self.coordinates) {
            for (i, span) in spans.into_iter().enumerate() {
                if previous[i] != xy[i] {
                    edits.push(Patch::replace(span, xy[i].to_string()));
                }
            }
            return Ok(());
        }
        let Span { start, end } = self.span;
        let mut prefix = format!("@position({}, {})", xy[0], xy[1]);
        if definition {
            let before = &source[..start as usize];
            let line = before.rsplit('\n').next().unwrap_or("");
            prefix.push_str(if source.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            });
            prefix.extend(line.chars().take_while(|c| matches!(c, ' ' | '\t')));
        } else if atom || self.before_binding {
            prefix.push(' ');
        } else {
            prefix.push_str(" (");
            edits.push(Patch {
                span: Span::new(end, end),
                text: ")".into(),
                enclosing_end: u32::MAX,
            });
        }
        edits.push(Patch {
            span: Span::new(start, start),
            text: prefix,
            enclosing_end: end,
        });
        Ok(())
    }
}

pub(super) struct Patch {
    span: Span,
    text: String,
    // Wrappers may open at the same byte (e.g. a call and its pipeline).
    enclosing_end: u32,
}

impl Patch {
    pub fn replace(span: Span, text: String) -> Self {
        Self {
            span,
            text,
            enclosing_end: 0,
        }
    }

    pub fn apply(mut patches: Vec<Self>, mut source: String) -> Result<String, String> {
        patches.sort_by_key(|p| (p.span.start, p.span.end, std::cmp::Reverse(p.enclosing_end)));
        if patches
            .windows(2)
            .any(|pair| pair[0].span.end > pair[1].span.start)
        {
            return Err("Two edits target the same source.".into());
        }
        for p in patches.into_iter().rev() {
            source.replace_range(p.span.start as usize..p.span.end as usize, &p.text);
        }
        Ok(source)
    }
}
