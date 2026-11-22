//! Reuse typed values through syntax-derived construction/projection relationships.
//! Source locations and captures distinguish candidates that were originally
//! equal. This cache is never used as activation or patch identity.
use super::*;
#[derive(Default)]
pub(super) struct SourceValues {
    keys: BTreeMap<String, usize>,
    nodes: Vec<ValueNode>,
    bytes: usize,
    candidate: bool,
}
struct ValueNode {
    value: Option<TypedValue>,
    projection: Option<(usize, String)>,
    has_children: bool,
}
impl Walker<'_> {
    pub(super) fn has_candidate_values(&self) -> bool {
        self.source_values.candidate
    }
    pub(super) fn render_site_typed(
        &self,
        site: &Site,
        prefix: &str,
    ) -> Result<(String, String), EditError> {
        if self.source_values.candidate {
            syntax::render_patched_typed(
                site,
                &self.scopes,
                &self.replacements,
                &self.snapshot.graph.entry().program.canonical.name,
                prefix,
            )
        } else {
            syntax::render_typed(site, &self.scopes, prefix)
        }
    }
    /// Exact top-level value bodies already have checked executable bindings.
    /// Candidate installation selects their revised code; lexical activations
    /// and synthetic/projection sites must still use their own captured source.
    fn global_value_source(&self, site: &Site) -> Option<String> {
        // Atoms have no computation to share. Keeping them inline also lets
        // identical literals/aliases share prepared query code across bindings.
        if matches!(
            site.expr.node,
            CExpr::IntLit(_)
                | CExpr::FloatLit(_)
                | CExpr::StringLit(_)
                | CExpr::Unit
                | CExpr::Var(_)
                | CExpr::Ctor(_)
                | CExpr::Hole
        ) {
            return None;
        }
        if site.scope != 0
            || site.synthetic
            || site.candidate_source
            || site.recipe.is_some()
            || !site.projections.is_empty()
        {
            return None;
        }
        let program = self
            .snapshot
            .graph
            .modules
            .iter()
            .map(|module| module.program.as_ref())
            .chain(self.bundled.iter())
            .find(|p| p.canonical.name == site.module)?;
        program
            .canonical
            .decls
            .iter()
            .find_map(|decl| match &decl.node {
                CDecl::Fn(def)
                    if def.params.is_empty()
                        && def.body.span == site.expr.span
                        && def.body == site.expr =>
                {
                    Some(global_source(&GlobalName {
                        module: site.module.clone(),
                        name: def.name.clone(),
                    }))
                }
                _ => None,
            })
    }
    fn value_source(&self, site: &Site, candidate: bool) -> Result<String, EditError> {
        let (source, helpers) = if let Some(global) = self.global_value_source(site) {
            (global, String::new())
        } else if candidate {
            syntax::render_patched_typed(
                site,
                &self.scopes,
                &self.replacements,
                &self.snapshot.graph.entry().program.canonical.name,
                "twineSourceValueType",
            )?
        } else {
            syntax::render_typed(site, &self.scopes, "twineSourceValueType")?
        };
        Ok(format!(
            "{helpers}{}current =\n{}",
            site.ty
                .as_ref()
                .map(|t| format!("current :: {}\n", t.source()))
                .unwrap_or_default(),
            indent(&source)
        ))
    }
    fn value_key(&self, site: &Site) -> Result<String, EditError> {
        Ok(format!(
            "{:?}:{}:{}:{}:{}:{}",
            site.module,
            site.expr.span.start,
            site.expr.span.end,
            site.synthetic,
            site.scope,
            self.value_source(site, false)?
        ))
    }
    fn value_node(&mut self, key: String) -> Option<usize> {
        if let Some(id) = self.source_values.keys.get(&key) {
            return Some(*id);
        }
        if self.source_values.nodes.len() >= self.probes.limits.traversal.retained_values
            || self.source_values.bytes.saturating_add(key.len()) > self.probes.limits.source_bytes
        {
            return None;
        }
        self.source_values.bytes += key.len();
        let id = self.source_values.nodes.len();
        self.source_values.keys.insert(key, id);
        self.source_values.nodes.push(ValueNode {
            value: None,
            projection: None,
            has_children: false,
        });
        Some(id)
    }
    pub(super) fn begin_candidate_values(&mut self) {
        self.source_values.candidate = true;
        for node in &mut self.source_values.nodes {
            node.value = None;
        }
    }
    pub(super) fn source_value(&mut self, site: &Site) -> Result<TypedValue, EditError> {
        let key = self.value_key(site)?;
        let id = self.value_node(key);
        if let Some(value) = id.and_then(|i| self.source_values.nodes[i].value.as_ref()) {
            return Ok(value.clone());
        }
        let projection = id
            .and_then(|i| self.source_values.nodes[i].projection.clone())
            .and_then(|(parent, expression)| {
                self.source_values.nodes[parent]
                    .value
                    .clone()
                    .map(|value| (value, expression))
            });
        let value = if let Some((parent, projection)) = projection {
            if let Some(output) = &site.ty {
                // The projection is closed except for its explicit parent.
                // Reuse the checked callable, passing each actual parent lazily.
                self.probes.declarations.clear();
                self.probes.call_typed(
                    &format!("\\parent ->\n{}", indent(&projection)),
                    &[parent],
                    output,
                )?
            } else {
                // Unknown field types still need contextual inference.
                let reference = self.probes.reference(&parent)?;
                self.probes.declarations = format!(
                    "parent :: {}\nparent = {reference}\n",
                    Ty::from_value(parent.value_type()).source()
                );
                self.probes.evaluate_lazy(&projection, None)?.0
            }
        } else {
            self.probes.declarations = self.value_source(site, self.source_values.candidate)?;
            self.probes.evaluate_lazy("current", site.ty.as_ref())?.0
        };
        if let Some(id) = id {
            self.source_values.nodes[id].value = Some(value.clone());
        }
        Ok(value)
    }
    /// Obtain a call's inputs lazily, sharing them with subsequent source visits.
    /// New inputs share one checked query; retained inputs keep their exact
    /// captures. Projecting the product forces only its shell, not the inputs.
    pub(super) fn source_arguments(
        &mut self,
        sites: &[Site],
    ) -> Result<Vec<TypedValue>, EditError> {
        let mut values = vec![None; sites.len()];
        let mut pending = Vec::new();
        let mut declarations = String::new();
        let mut names = Vec::new();
        let mut types = Vec::new();
        for (i, site) in sites.iter().enumerate() {
            if self.can_reuse_value(site)? {
                values[i] = Some(self.source_value(site)?);
            } else {
                let name = format!("argument{i}");
                declarations.push_str(&self.value_declaration(
                    site,
                    &name,
                    &format!("twineInput{i}Type"),
                )?);
                names.push(name);
                types.push(
                    site.ty
                        .clone()
                        .ok_or_else(|| EditError::Internal("unchecked call argument".into()))?,
                );
                pending.push(i);
            }
        }
        if !pending.is_empty() {
            let ty = if types.len() == 1 {
                types.pop().unwrap()
            } else {
                Ty::Tuple(types)
            };
            self.probes.declarations = declarations;
            let (product, _) = self.probes.evaluate_lazy(&group(&names), Some(&ty))?;
            for (field, &i) in pending.iter().enumerate() {
                let value = if pending.len() == 1 {
                    product.clone()
                } else {
                    self.probes.project_live(&product, Field::Index(field))?
                };
                let key = self.value_key(&sites[i])?;
                if let Some(id) = self.value_node(key) {
                    self.source_values.nodes[id].value = Some(value.clone());
                }
                values[i] = Some(value);
            }
        }
        Ok(values.into_iter().map(Option::unwrap).collect())
    }
    fn can_reuse_value(&self, site: &Site) -> Result<bool, EditError> {
        let key = self.value_key(site)?;
        Ok(self.source_values.keys.get(&key).is_some_and(|i| {
            let node = &self.source_values.nodes[*i];
            node.value.is_some()
                || node.has_children
                || node
                    .projection
                    .as_ref()
                    .is_some_and(|(parent, _)| self.source_values.nodes[*parent].value.is_some())
        }))
    }
    pub(super) fn value_declaration(
        &mut self,
        site: &Site,
        name: &str,
        prefix: &str,
    ) -> Result<String, EditError> {
        if self.can_reuse_value(site)? {
            let value = self.source_value(site)?;
            let reference = self.probes.reference(&value)?;
            return Ok(format!(
                "{name} :: {}\n{name} = {reference}\n",
                Ty::from_value(value.value_type()).source()
            ));
        }
        // Without a known reusable value path, bind this expression directly in
        // its caller. No extra checked operation is needed merely to name it.
        let (value, helpers) = if let Some(global) = self.global_value_source(site) {
            (global, String::new())
        } else if self.source_values.candidate {
            syntax::render_patched_typed(
                site,
                &self.scopes,
                &self.replacements,
                &self.snapshot.graph.entry().program.canonical.name,
                prefix,
            )?
        } else {
            syntax::render_typed(site, &self.scopes, prefix)?
        };
        Ok(format!(
            "{helpers}{}{name} =\n{}",
            site.ty
                .as_ref()
                .map(|t| format!("{name} :: {}\n", t.source()))
                .unwrap_or_default(),
            indent(&value)
        ))
    }
    pub(super) fn current_declaration(&mut self, site: &Site) -> Result<String, EditError> {
        self.value_declaration(site, "current", "twineCurrentType")
    }
    fn remember_projection(
        &mut self,
        parent: usize,
        child: &Site,
        expression: String,
    ) -> Result<(), EditError> {
        let key = self.value_key(child)?;
        if let Some(id) = self.value_node(key) {
            if id != parent
                && self.source_values.nodes[id].projection.is_none()
                && self.source_values.bytes.saturating_add(expression.len())
                    <= self.probes.limits.source_bytes
            {
                self.source_values.bytes += expression.len();
                self.source_values.nodes[id].projection = Some((parent, expression));
                self.source_values.nodes[parent].has_children = true;
            }
        }
        Ok(())
    }
    pub(super) fn remember_product_fields(
        &mut self,
        parent: &Site,
        children: &[Site],
    ) -> Result<(), EditError> {
        let key = self.value_key(parent)?;
        let Some(parent_id) = self.value_node(key) else {
            return Ok(());
        };
        for (i, child) in children.iter().enumerate() {
            let projection = match (&parent.expr.node, parent.ty.as_ref()) {
                (CExpr::Tuple(_), Some(Ty::Tuple(_))) => {
                    let names = (0..children.len())
                        .map(|j| {
                            if i == j {
                                "selected".into()
                            } else {
                                "_".into()
                            }
                        })
                        .collect::<Vec<_>>();
                    format!("case parent of\n  {} -> selected", group(&names))
                }
                (CExpr::Record(fields), Some(Ty::Record(_))) => {
                    // Custom rules order feeds by label, not source position.
                    let (name, _) = fields
                        .iter()
                        .find(|(_, expression)| expression.span == child.expr.span)
                        .ok_or_else(|| {
                            EditError::Internal("record child source identity".into())
                        })?;
                    format!("parent.{name}")
                }
                (_, Some(Ty::Record(fields))) => {
                    format!("parent.{}", fields.keys().nth(i).unwrap())
                }
                _ => {
                    return Err(EditError::Internal(
                        "original product projection shape".into(),
                    ))
                }
            };
            self.remember_projection(parent_id, child, projection)?;
        }
        self.source_value(parent)?;
        Ok(())
    }
    pub(super) fn remember_constructor_fields(
        &mut self,
        parent: &Site,
        constructor: &GlobalName,
        children: &[Site],
    ) -> Result<(), EditError> {
        let key = self.value_key(parent)?;
        let Some(parent_id) = self.value_node(key) else {
            return Ok(());
        };
        for (i, child) in children.iter().enumerate() {
            let names = (0..children.len())
                .map(|j| {
                    if i == j {
                        "selected".into()
                    } else {
                        "_".into()
                    }
                })
                .collect::<Vec<_>>();
            self.remember_projection(parent_id,child,format!("case parent of\n  {} -> selected\n  _ -> error \"construction changed variant\"",constructor_rules::constructor_pattern(constructor,&names)))?;
        }
        self.source_value(parent)?;
        Ok(())
    }
}
