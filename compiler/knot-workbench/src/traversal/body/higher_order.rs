//! Nested companions remain ordinary closures. Their local helpers capture the
//! enclosing original input; type identities retain every generated signature.
use super::*;
use crate::traversal::higher_order::{PreparedCall, SourceCall};

impl Compiler<'_, '_> {
    pub(super) fn higher_order(
        &mut self,
        call: call_frame::CheckedCall,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        for arg in &call.args {
            if !matches!(arg.ty, Some(Ty::Function(_, _))) {
                continue;
            }
            let dependency = self.dependency_source(arg)?;
            if dependency.contains(&self.symbol)
                || self.multi.as_ref().is_some_and(|inputs| {
                    inputs
                        .symbols
                        .iter()
                        .any(|symbol| dependency.contains(symbol))
                })
            {
                return unsupported("nested callback captures its enclosing editable input; capture provenance is required");
            }
        }
        let rules = call.rules.clone();
        let PreparedCall {
            call,
            policies,
            forward_only,
            generated,
            selected,
            input,
            rule_type,
            plain,
        } = self.walker.prepare_companions(
            SourceCall {
                name: call.name,
                args: call.args,
                actuals: call.actuals,
                declarations: call.declarations,
                output: call.output,
                request_type: request.clone(),
                rules: rules.len(),
                bodies: call.bodies,
            },
            &self.metadata(),
        )?;
        let mut selected_rule = rules[selected].clone();
        selected_rule.ty = Some(rule_type);
        self.walker.pin_source_dependencies(selected_rule)?;
        let rule = self.walker.source(&rules[selected])?;
        let prefix = format!("{forward}Companion");
        let (locals, identities) = local_helpers(&generated, &prefix)?;
        self.declarations.push_str(&identities);
        let mut bindings = format!("{} = input.value\n{locals}", self.symbol);
        let mut companions = Vec::new();
        for i in 0..call.args.len() {
            if !policies.contains_key(&i) && !forward_only.contains(&i) {
                continue;
            }
            let (actual, helpers) = self
                .walker
                .render_site_typed(&call.args[i], &format!("{prefix}Actual{i}Type"))?;
            self.declarations.push_str(&helpers);
            let companion = format!("{prefix}{i}");
            match policies.get(&i) {
                Some(factory) => bindings.push_str(&format!(
                    "{companion} = {{ forward = ({}), unravel = ({factory}) ({}) }}\n",
                    actual, actual
                )),
                // The rule takes this callback as the plain function.
                None => bindings.push_str(&format!("{companion} = ({actual})\n")),
            }
            companions.push(companion);
        }
        let args = call
            .args
            .into_iter()
            .enumerate()
            .filter_map(|(i, arg)| {
                (!policies.contains_key(&i) && !forward_only.contains(&i)).then_some(arg)
            })
            .collect();
        let args: Vec<Site> = args;
        let body = self.unravel_apply(
            &call_frame::feeds_rule(
                &format!("({rule}) {}", companions.join(" ")),
                args.len(),
                plain,
            ),
            args,
            input,
            forward,
        )?;
        Ok(format!("let\n{}in\n{}", indent(&bindings), indent(&body)))
    }
}

/// Local lets have no signature syntax. Apply a monomorphic, globally checked
/// identity to each local function instead of dropping its declared evidence.
fn local_helpers(source: &str, prefix: &str) -> Result<(String, String), EditError> {
    if source.is_empty() {
        return Ok((String::new(), String::new()));
    }
    let source = format!("module NestedBody exposing (..)\n{source}");
    let parsed = knot_syntax::parse(&source).map_err(|e| EditError::Parse(e.render(&source)))?;
    let mut locals = String::new();
    let mut identities = String::new();
    for (index, decl) in parsed.decls.iter().enumerate() {
        let knot_syntax::ast::decl::Decl::Fn(f) = &decl.node else {
            return Err(EditError::Internal(
                "generated body has a non-function declaration".into(),
            ));
        };
        let Some(signature) = &f.signature else {
            return Err(EditError::Internal(
                "generated body helper has no signature".into(),
            ));
        };
        let slice = |span: knot_syntax::Span| &source[span.start as usize..span.end as usize];
        let ty = slice(signature.span)
            .trim()
            .strip_prefix("::")
            .ok_or_else(|| EditError::Internal("generated signature prefix".into()))?
            .trim();
        let identity = format!("{prefix}Type{index}");
        identities.push_str(&format!(
            "{identity} :: ({ty}) -> ({ty})\n{identity} value = value\n"
        ));
        // Preserve the complete definition and parameter spelling, including
        // pattern grouping, rather than reconstructing declaration syntax.
        let end = parsed
            .decls
            .get(index + 1)
            .map_or(source.len(), |d| d.span.start as usize);
        let definition = source[signature.span.end as usize..end].trim();
        let (header, body) = definition
            .split_once('=')
            .ok_or_else(|| EditError::Internal("generated function definition".into()))?;
        let params = header
            .trim()
            .strip_prefix(&f.name)
            .ok_or_else(|| EditError::Internal("generated function name".into()))?
            .trim();
        let body = if body.lines().next().unwrap_or("").trim().is_empty() {
            let margin = body
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| line.len() - line.trim_start().len())
                .min()
                .unwrap_or(0);
            body.lines()
                .map(|line| line.get(margin..).unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            body.to_owned()
        };
        let expression = if params.is_empty() {
            body.trim().to_owned()
        } else {
            format!("\\{params} ->\n{}", indent(body.trim()))
        };
        locals.push_str(&format!(
            "{} = {identity} (\n{}\n)\n",
            f.name,
            indent(&expression)
        ));
    }
    Ok((locals, identities))
}
