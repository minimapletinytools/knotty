//! Alpha-normalize only generated retained-value bindings and their resolved
//! top-level uses. Strings, patterns, shadowed locals, types and all policy code
//! remain exact. The result is a cache key, never executable source.
use super::*;
use knot_canonical::ast::{CDoStmt, CModule};
pub(super) fn key(
    source: &str,
    module: &CModule,
    references: &[(String, TypedValue)],
    module_name: &str,
    operation: &str,
) -> Option<String> {
    normalized_key(source, module, references, module_name, Some(operation))
}

/// Typechecking metadata can also ignore generated argument names. Their
/// declared types and all user syntax remain part of the key. Resolve names
/// first so shadowed locals and strings cannot be mistaken for arguments.
pub(super) fn metadata_key(
    source: &str,
    module: &CModule,
    references: &[(String, TypedValue)],
) -> Option<String> {
    normalized_key(source, module, references, "TwineTraversalQuery", None)
        .map(|key| format!("metadata:{key}"))
}

fn normalized_key(
    source: &str,
    module: &CModule,
    references: &[(String, TypedValue)],
    module_name: &str,
    operation: Option<&str>,
) -> Option<String> {
    let mut replacements: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let start = source.find(&format!("module {module_name} "))? + 7;
    replacements.insert((start, start + module_name.len()), "$module".into());
    if let Some(operation) = operation {
        let start = source.rfind(&format!("\n{operation} = probe "))? + 1;
        replacements.insert((start, start + operation.len()), "$entry".into());
    }
    let mut names = BTreeMap::new();
    for (i, (name, _)) in references.iter().enumerate() {
        let placeholder = format!("$argument{i}");
        names.insert(name.as_str(), placeholder.clone());
        for suffix in [" :: ", " = undefined\n"] {
            let start = source.find(&format!("\n{name}{suffix}"))? + 1;
            replacements.insert((start, start + name.len()), placeholder.clone());
        }
    }
    let mut pending = Vec::new();
    let mut shape = String::new();
    for declaration in &module.decls {
        if let CDecl::Fn(f) = &declaration.node {
            shape.push_str(&format!("fn{}:{};", f.params.len(), f.annotations.len()));
            pending.push(&f.body);
            pending.extend(f.annotations.iter().map(|a| &a.value));
        }
    }
    while let Some(expr) = pending.pop() {
        let before = pending.len();
        shape.push_str(&format!("{:?}:", std::mem::discriminant(&expr.node)));
        match &expr.node {
            CExpr::Var(Ref::TopLevel(name)) => {
                if let Some(replacement) = names.get(name.as_str()) {
                    let (start, end) = identifier_span(source, expr.span, name)?;
                    replacements.insert((start, end), replacement.clone());
                }
            }
            CExpr::App(a, b) | CExpr::BinOp(_, a, b) => {
                pending.push(a);
                pending.push(b);
            }
            CExpr::Lambda(_, body) | CExpr::Negate(body) | CExpr::FieldAccess(body, _) => {
                pending.push(body)
            }
            CExpr::If(c, a, b) => {
                pending.push(c);
                pending.push(a);
                pending.push(b);
            }
            CExpr::Let(bindings, body) => {
                pending.push(body);
                pending.extend(bindings.iter().map(|(_, e)| e));
            }
            CExpr::Case(value, arms) => {
                pending.push(value);
                pending.extend(arms.iter().map(|(_, e)| e));
            }
            CExpr::Do(statements, body) => {
                pending.push(body);
                for statement in statements {
                    match statement {
                        CDoStmt::Let(bindings) => pending.extend(bindings.iter().map(|(_, e)| e)),
                        CDoStmt::Bind(_, e) | CDoStmt::Expr(e) => pending.push(e),
                    }
                }
            }
            CExpr::List(values) | CExpr::Tuple(values) => pending.extend(values),
            CExpr::Record(fields) => pending.extend(fields.iter().map(|(_, e)| e)),
            CExpr::RecordUpdate(base, fields) => {
                pending.push(base);
                pending.extend(fields.iter().map(|(_, e)| e));
            }
            CExpr::Annotated(annotations, body) => {
                pending.push(body);
                pending.extend(annotations.iter().map(|a| &a.value));
            }
            _ => {}
        }
        shape.push_str(&format!("{};", pending.len() - before));
    }
    let mut key = source.to_owned();
    for ((start, end), replacement) in replacements.into_iter().rev() {
        key.replace_range(start..end, &replacement);
    }
    Some(format!("{}:{key}{shape}", key.len()))
}

// Grouping belongs to the expression span, not to the referenced identifier.
// Keep parentheses/layout in the cache key and normalize only the binding token.
// Comments or any unexpected spelling safely disable pooling for this query.
fn identifier_span(source: &str, span: knot_syntax::Span, name: &str) -> Option<(usize, usize)> {
    let mut start = span.start as usize;
    let mut end = span.end as usize;
    loop {
        let text = source.get(start..end)?;
        start += text.len() - text.trim_start().len();
        end = start + text.trim().len();
        let text = source.get(start..end)?;
        if text == name {
            return Some((start, end));
        }
        if !(text.starts_with('(') && text.ends_with(')')) {
            return None;
        }
        start += 1;
        end -= 1;
    }
}
