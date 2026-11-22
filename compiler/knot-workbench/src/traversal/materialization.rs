//! Concrete candidates use a bounded value arena, not recursive decoded trees.
//! Deep values print as ordinary local bindings so parser/checker stack depth
//! does not grow with a collection's length. No collection names select policy.
use super::*;
use knot_canonical::ast::CModule;
use std::collections::HashMap;
use twine::{Session, ThunkHandle, ValueHandle, ValueView};
pub(super) struct Materialized {
    pub edit: String,
}
enum Shape {
    Literal(knot_core::Literal),
    Constructor(GlobalName),
    Tuple,
    Record(Vec<String>),
}
struct Node {
    shape: Shape,
    children: Vec<usize>,
    depth: usize,
}
enum Work {
    Value(ValueHandle),
    Force(ThunkHandle),
    Finish(ValueHandle, usize, usize),
}
pub(super) fn read(
    session: &mut Session,
    root: ValueHandle,
    module: &CModule,
    remaining: &mut usize,
    bytes: usize,
) -> Result<Materialized, EditError> {
    let mut nodes = Vec::<Node>::new();
    let mut known = HashMap::<ValueHandle, (usize, bool)>::new();
    let mut pending = vec![Work::Value(root)];
    let mut results = Vec::new();
    let mut order = Vec::new();
    let mut retained = 0usize;
    while let Some(work) = pending.pop() {
        match work {
            Work::Force(thunk) => pending.push(Work::Value(session.force(thunk)?)),
            Work::Value(value) => {
                if let Some((id, active)) = known.get(&value) {
                    if *active {
                        return unsupported("cyclic values need an explicit source recipe");
                    }
                    results.push(*id);
                    continue;
                }
                *remaining = remaining.checked_sub(1).ok_or_else(|| {
                    EditError::Unsupported(
                        "candidate materialization exceeded its value budget".into(),
                    )
                })?;
                let view = session.view(value)?;
                let storage = match &view {
                    ValueView::Literal(knot_core::Literal::String(s)) => s.len(),
                    ValueView::Literal(knot_core::Literal::Integer(n)) => {
                        n.bits().div_ceil(8) as usize
                    }
                    ValueView::Constructor(n, count) => n
                        .name
                        .len()
                        .saturating_add(n.module.iter().map(String::len).sum::<usize>())
                        .saturating_add(count.saturating_mul(std::mem::size_of::<usize>())),
                    ValueView::Tuple(count) => count.saturating_mul(std::mem::size_of::<usize>()),
                    ValueView::Record(fields) => fields
                        .iter()
                        .map(|f| f.len().saturating_add(std::mem::size_of::<usize>()))
                        .sum(),
                    _ => 16,
                };
                retained = retained.saturating_add(64).saturating_add(storage);
                if retained > bytes {
                    return Err(EditError::SourceTooLarge);
                }
                let (shape, count) = match view {
                    ValueView::Literal(l) => (Shape::Literal(l.clone()), 0),
                    ValueView::Constructor(n, count) => (Shape::Constructor(n.clone()), count),
                    ValueView::Tuple(count) => (Shape::Tuple, count),
                    ValueView::Record(fields) => (
                        Shape::Record(fields.iter().map(|s| s.to_string()).collect()),
                        fields.len(),
                    ),
                    _ => {
                        return unsupported(
                            "functions, native values and IO have no source materializer",
                        )
                    }
                };
                let id = nodes.len();
                nodes.push(Node {
                    shape,
                    children: vec![],
                    depth: 1,
                });
                known.insert(value, (id, true));
                pending.push(Work::Finish(value, id, count));
                for child in session.children(value, 0, count)?.into_iter().rev() {
                    pending.push(Work::Force(child.handle));
                }
            }
            Work::Finish(value, id, count) => {
                let children = results.split_off(results.len() - count);
                nodes[id].depth = children.iter().map(|i| nodes[*i].depth).max().unwrap_or(0) + 1;
                nodes[id].children = children;
                known.insert(value, (id, false));
                results.push(id);
                order.push(id);
            }
        }
    }
    let root = results
        .pop()
        .ok_or_else(|| EditError::Internal("empty materialized graph".into()))?;
    let flat = nodes[root].depth > 32 || nodes.len() > 128;
    Ok(Materialized {
        edit: render(&nodes, &order, root, flat, module, bytes)?,
    })
}
pub(super) fn constructor(name: &GlobalName, module: &CModule) -> Result<String, EditError> {
    if name.module == ["Prelude"] {
        return Ok(global_source(name));
    }
    if name.module == module.name {
        return Ok(name.name.clone());
    }
    let import = module
        .imports
        .iter()
        .find(|i| i.module == name.module)
        .ok_or_else(|| {
            EditError::Unsupported(format!("no imported constructor recipe for {name:?}"))
        })?;
    Ok(format!(
        "{}.{}",
        import
            .alias
            .clone()
            .unwrap_or_else(|| name.module.join(".")),
        name.name
    ))
}
fn append(out: &mut String, text: &str, limit: usize) -> Result<(), EditError> {
    if out.len().saturating_add(text.len()) > limit {
        return Err(EditError::SourceTooLarge);
    }
    out.push_str(text);
    Ok(())
}
fn node_source(
    node: &Node,
    child: impl Fn(usize) -> String,
    module: &CModule,
    bytes: usize,
) -> Result<String, EditError> {
    let mut out = String::new();
    match &node.shape {
        Shape::Literal(l) => append(&mut out, &Data::Literal(l.clone()).source()?, bytes)?,
        Shape::Constructor(name) => {
            append(&mut out, "(", bytes)?;
            append(&mut out, &constructor(name, module)?, bytes)?;
            for id in &node.children {
                append(&mut out, " ", bytes)?;
                append(&mut out, &child(*id), bytes)?;
            }
            append(&mut out, ")", bytes)?;
        }
        Shape::Tuple => {
            append(&mut out, "(", bytes)?;
            for (i, id) in node.children.iter().enumerate() {
                if i > 0 {
                    append(&mut out, ", ", bytes)?;
                }
                append(&mut out, &child(*id), bytes)?;
            }
            append(&mut out, ")", bytes)?;
        }
        Shape::Record(fields) => {
            append(&mut out, "{ ", bytes)?;
            for (i, (field, id)) in fields.iter().zip(&node.children).enumerate() {
                if i > 0 {
                    append(&mut out, ", ", bytes)?;
                }
                append(&mut out, field, bytes)?;
                append(&mut out, " = ", bytes)?;
                append(&mut out, &child(*id), bytes)?;
            }
            append(&mut out, " }", bytes)?;
        }
    }
    Ok(out)
}
fn render(
    nodes: &[Node],
    order: &[usize],
    root: usize,
    flat: bool,
    module: &CModule,
    bytes: usize,
) -> Result<String, EditError> {
    if flat {
        let mut leaves = HashMap::new();
        for id in order {
            if nodes[*id].children.is_empty() {
                leaves.insert(
                    *id,
                    node_source(&nodes[*id], |_| unreachable!(), module, bytes)?,
                );
            }
        }
        let mut out = String::from("(let\n");
        for id in order {
            if leaves.contains_key(id) {
                continue;
            }
            let value = node_source(
                &nodes[*id],
                |id| {
                    leaves
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("twineMaterial{id}"))
                },
                module,
                bytes,
            )?;
            append(&mut out, &format!("  twineMaterial{id} = {value}\n"), bytes)?;
        }
        append(&mut out, &format!("in twineMaterial{root})"), bytes)?;
        return Ok(out);
    }
    let mut sources = vec![String::new(); nodes.len()];
    let mut retained = 0usize;
    for id in order {
        let value = node_source(&nodes[*id], |id| sources[id].clone(), module, bytes)?;
        retained = retained.saturating_add(value.len());
        if retained > bytes {
            return Err(EditError::SourceTooLarge);
        }
        sources[*id] = value;
    }
    Ok(std::mem::take(&mut sources[root]))
}
pub(super) fn at_column(
    value: &str,
    source: &str,
    start: usize,
    bytes: usize,
) -> Result<String, EditError> {
    let before = &source[..start];
    let column = before
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .chars()
        .count();
    let extra = value
        .bytes()
        .filter(|c| *c == b'\n')
        .count()
        .saturating_mul(column);
    if value.len().saturating_add(extra) > bytes {
        return Err(EditError::SourceTooLarge);
    }
    Ok(value.replace('\n', &format!("\n{}", " ".repeat(column))))
}
