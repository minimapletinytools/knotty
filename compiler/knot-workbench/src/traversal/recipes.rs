//! Source insertion uses construction contexts with preserved expression fields.
//! Policies supply concrete candidates; recipes neither inspect constraints nor
//! choose factories. Physical parent rewrites retain logical edit footprints.
use super::*;
use knot_syntax::Span;
mod text;
use text::Text;
pub(super) type Locations = BTreeMap<(u32, u32), Vec<((u32, u32), Span)>>;
pub(super) struct Construction {
    pub patches: Vec<LiteralPatch>,
    pub locations: Locations,
}

pub(super) struct Recipe {
    pub root: Arc<Site>,
    parent: Option<Arc<Recipe>>,
    constructor: GlobalName,
    before: Vec<Site>,
    after: Vec<Site>,
    input: Option<Ty>,
    affected: Span,
    depth: usize,
}
impl Drop for Recipe {
    fn drop(&mut self) {
        let mut parent = self.parent.take();
        while let Some(previous) = parent {
            let Ok(mut previous) = Arc::try_unwrap(previous) else {
                break;
            };
            parent = previous.parent.take();
        }
    }
}
impl Recipe {
    pub fn field(
        parent: &Site,
        constructor: GlobalName,
        before: Vec<Site>,
        after: Vec<Site>,
        affected: Span,
    ) -> Option<Arc<Self>> {
        if parent.synthetic && parent.recipe.is_none() {
            return None;
        }
        let root = parent
            .recipe
            .as_ref()
            .map_or_else(|| Arc::new(parent.clone()), |r| r.root.clone());
        Some(Arc::new(Self {
            root,
            parent: parent.recipe.clone(),
            constructor,
            before,
            after,
            input: parent.ty.clone(),
            affected,
            depth: parent.recipe.as_ref().map_or(1, |r| r.depth + 1),
        }))
    }
    pub(super) fn projection_identity(&self) -> String {
        self.steps()
            .into_iter()
            .map(|step| format!("{:?}/{}", step.constructor, step.before.len()))
            .collect::<Vec<_>>()
            .join(";")
    }
    fn steps(&self) -> Vec<&Self> {
        let mut steps = Vec::new();
        let mut step = Some(self);
        while let Some(current) = step {
            steps.push(current);
            step = current.parent.as_deref();
        }
        steps.reverse();
        steps
    }
    /// Project from the candidate parent, including any newly inserted fields.
    /// Flat lets keep long source paths off the parser/checker's native stack.
    pub fn project(
        &self,
        root: String,
        typing: Option<&syntax::Typing>,
    ) -> Result<String, EditError> {
        let mut prefix = "twineRecipeProjection".to_string();
        while root.contains(&prefix) {
            prefix.push('x');
        }
        let mut bindings = format!("{prefix}0 =\n{}", indent(&root));
        for (i, step) in self.steps().into_iter().enumerate() {
            let projection = projections::Projection {
                input: step.input.clone(),
                kind: ProjectionKind::Constructor {
                    name: step.constructor.clone(),
                    arity: step.before.len() + 1 + step.after.len(),
                    index: step.before.len(),
                },
            };
            let value = projections::render(format!("{prefix}{i}"), &[projection], typing)?;
            bindings.push_str(&format!("{prefix}{} =\n{}", i + 1, indent(&value)));
            if bindings.len() > 1024 * 1024 {
                return Err(EditError::SourceTooLarge);
            }
        }
        Ok(format!(
            "(let\n{}in {prefix}{})",
            indent(&bindings),
            self.depth
        ))
    }
}
#[derive(Clone)]
pub(super) struct PendingRecipe {
    recipe: Arc<Recipe>,
    value: String,
}
impl Walker<'_> {
    pub(super) fn materialized_assignment(&self, site: &Site) -> bool {
        if let Some(recipe) = &site.recipe {
            return self
                .recipes
                .get(&(recipe.root.expr.span.start, recipe.root.expr.span.end))
                .is_some_and(|pending| {
                    pending.recipe.projection_identity() == recipe.projection_identity()
                });
        }
        self.patches
            .contains_key(&(site.expr.span.start, site.expr.span.end))
    }
    pub(super) fn materialize_recipe(
        &mut self,
        site: &Site,
        request: &TypedValue,
        ty: &Ty,
    ) -> Result<(), EditError> {
        let recipe = site.recipe.clone().ok_or_else(|| {
            EditError::Unsupported(
                "synthetic aggregate needs an explicit source insertion recipe".into(),
            )
        })?;
        let root = &recipe.root;
        if site.locked
            || site.fixed
            || root.locked
            || root.fixed
            || root.module != self.snapshot.graph.entry().program.canonical.name
        {
            return unsupported("this source construction is pinned or read-only");
        }
        let value = self.choose_candidate(site, request, ty)?.edit;
        let key = (root.expr.span.start, root.expr.span.end);
        if let Some(previous) = self.recipes.get(&key) {
            if previous.recipe.affected != recipe.affected || previous.value != value {
                return Err(EditError::ConflictingRequests);
            }
            return Ok(());
        }
        self.changed_spans
            .insert((recipe.affected.start, recipe.affected.end));
        self.stepped = true;
        self.recipes.insert(key, PendingRecipe { recipe, value });
        Ok(())
    }
    pub(super) fn finish_recipes(&self) -> Result<Construction, EditError> {
        let mut patches = self.patches.clone();
        let mut locations = self.patch_locations.clone();
        let mut recipes = self.recipes.values().cloned().collect::<Vec<_>>();
        recipes.sort_by_key(|r| r.recipe.root.expr.span.end - r.recipe.root.expr.span.start);
        let source = &self.snapshot.sources()[self.snapshot.entry_path()];
        let module = &self.snapshot.graph.entry().program.canonical;
        for pending in recipes {
            let span = pending.recipe.root.expr.span;
            if patches.keys().any(|&(a, b)| {
                a < span.end
                    && span.start < b
                    && !(span.start <= a && b <= span.end && (a, b) != (span.start, span.end))
            }) {
                return unsupported("overlapping source insertion and replacement");
            }
            let value = pending.render(
                source,
                module,
                &patches,
                &locations,
                self.probes.limits.source_bytes,
            )?;
            let column = source[..span.start as usize]
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .chars()
                .count();
            let mut value = value.indented(column, false)?;
            let key = (span.start, span.end);
            value
                .locations
                .push((key, Span::new(0, value.text.len() as u32)));
            locations.insert(key, value.locations);
            let replacement = value.text;
            patches.retain(|&(a, b), _| !(span.start <= a && b <= span.end));
            patches.insert(
                (span.start, span.end),
                LiteralPatch {
                    span,
                    before: source[span.start as usize..span.end as usize].to_owned(),
                    replacement,
                },
            );
        }
        Ok(Construction {
            patches: patches.into_values().collect(),
            locations,
        })
    }
}
impl PendingRecipe {
    fn render(
        &self,
        source: &str,
        module: &knot_canonical::ast::CModule,
        patches: &BTreeMap<(u32, u32), LiteralPatch>,
        locations: &Locations,
        limit: usize,
    ) -> Result<Text, EditError> {
        let mut prefix = "twineConstruction".to_string();
        while source.contains(&prefix) || self.value.contains(&prefix) {
            prefix.push('x');
        }
        let mut definitions = Text::new("", limit)?;
        for (i, step) in self.recipe.steps().into_iter().enumerate() {
            let constructor = materialization::constructor(&step.constructor, module)?;
            definitions.push(&format!("{prefix}{i} = {constructor}\n"))?;
            let mut fields = Text::new("", limit)?;
            for field in &step.before {
                fields.append(fragment(field, source, patches, locations, limit)?)?;
                fields.push("\n")?;
            }
            fields.push(&format!("{prefix}{}", i + 1))?;
            for field in &step.after {
                fields.push("\n")?;
                fields.append(fragment(field, source, patches, locations, limit)?)?;
            }
            definitions.append(fields.indented(2, true)?)?;
            definitions.push("\n")?;
        }
        definitions.push(&format!("{prefix}{} =\n", self.recipe.depth))?;
        definitions.append(Text::new(&self.value, limit)?.indented(2, true)?)?;
        let mut expression = Text::new("(let\n", limit)?;
        expression.append(definitions.indented(2, true)?)?;
        expression.push(&format!("\nin {prefix}0)"))?;
        Ok(expression)
    }
}
fn fragment(
    site: &Site,
    source: &str,
    patches: &BTreeMap<(u32, u32), LiteralPatch>,
    locations: &Locations,
    limit: usize,
) -> Result<Text, EditError> {
    if site.synthetic {
        return unsupported("preserved recipe fields need original source expressions");
    }
    let span = site.expr.span;
    if source.get(span.start as usize..span.end as usize).is_none() {
        return Err(EditError::Internal("recipe field outside source".into()));
    }
    let column = source[..span.start as usize]
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .count();
    let mut value = Text::new(&" ".repeat(column), limit)?;
    let mut cursor = span.start;
    for (key, patch) in patches {
        if span.start <= patch.span.start && patch.span.end <= span.end {
            value.push(&source[cursor as usize..patch.span.start as usize])?;
            let mut replacement = Text::new(&patch.replacement, limit)?;
            replacement.locations = locations
                .get(key)
                .ok_or_else(|| EditError::Internal("missing preserved patch locations".into()))?
                .clone();
            value.append(replacement)?;
            cursor = patch.span.end;
        }
    }
    value.push(&source[cursor as usize..span.end as usize])?;
    // Restore the original first-line column, then shift the whole fragment
    // uniformly inside a parenthesized operand. Nested layout stays identical.
    let mut result = Text::new("(\n", limit)?;
    result.append(value.indented(2, true)?)?;
    result.push("\n)")?;
    // Transparent grouping expands the root expression's canonical span.
    for (original, target) in &mut result.locations {
        if *original == (span.start, span.end) {
            *target = Span::new(0, result.text.len() as u32);
        }
    }
    Ok(result)
}
