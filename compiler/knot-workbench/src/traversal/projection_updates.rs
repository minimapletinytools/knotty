//! Checked updates through structural projection paths, shared by source
//! traversal and generated callback bodies. Policy remains ordinary Knot code.
use super::*;

pub(super) fn compile(
    probes: &mut Probes,
    ambient: &str,
    path: &[projections::Projection],
    value: &Ty,
    leaf: &Ty,
    prefix: &str,
) -> Result<(String, String), EditError> {
    let mut compiler = Updates {
        probes,
        ambient,
        declarations: String::new(),
    };
    let name = compiler.setter(path, value, leaf, prefix, 0)?;
    Ok((name, compiler.declarations))
}
struct Updates<'a> {
    probes: &'a mut Probes,
    ambient: &'a str,
    declarations: String,
}
impl Updates<'_> {
    fn setter(
        &mut self,
        path: &[projections::Projection],
        value: &Ty,
        leaf: &Ty,
        prefix: &str,
        depth: usize,
    ) -> Result<String, EditError> {
        if depth > 128 {
            return unsupported("generated input projection exceeded 128 levels");
        }
        let name = format!("{prefix}{depth}");
        let old = format!("{name}Old");
        let new = format!("{name}New");
        let body = if let Some((first, rest)) = path.split_first() {
            let projected =
                projections::render("lensValue".into(), std::slice::from_ref(first), None)?;
            self.probes.declarations = self.ambient.to_owned();
            let checked = self.probes.check(&format!(
                "lensValue :: {}\nlensValue = undefined\nlensField =\n{}",
                value.source(),
                indent(&projected)
            ))?;
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel(
                    "lensField".into(),
                ))
                .unwrap();
            let child = Ty::read(&mut checked.substitution.clone(), scheme.ty)?;
            let nested = self.setter(rest, &child, leaf, prefix, depth + 1)?;
            match &first.kind {
                ProjectionKind::Field(field) => format!("do\n  changed <- {nested} {new} {old}.{field}\n  pure {{ {old} | {field} = changed }}"),
                ProjectionKind::Tuple { arity, index } | ProjectionKind::Constructor { arity, index, .. } => {
                    let fields = (0..*arity).map(|i| format!("{name}Field{i}")).collect::<Vec<_>>();
                    let Some(selected) = fields.get(*index) else { return Err(EditError::Internal("input projection field index".into())) };
                    let mut rebuilt = fields.clone(); rebuilt[*index] = "changed".into();
                    let (pattern, result, fallback) = match &first.kind {
                        ProjectionKind::Constructor { name, .. } => (constructor_rules::constructor_pattern(name, &fields), format!("{} {}", global_source(name), rebuilt.join(" ")), "\n  _ -> Nothing"),
                        _ => (group(&fields), group(&rebuilt), ""),
                    };
                    format!("case {old} of\n  {pattern} -> do\n    changed <- {nested} {new} {selected}\n    pure ({result}){fallback}")
                }
                ProjectionKind::Match { pattern, module } => format!("case {old} of\n  {} -> {nested} {new} {old}\n  _ -> Nothing", syntax::pattern(pattern, module)),
            }
        } else {
            if value != leaf {
                return Err(EditError::Internal("input projection result type".into()));
            }
            format!("Just {new}")
        };
        self.declarations.push_str(&format!(
            "{name} :: {} -> {} -> Maybe {}\n{name} {new} {old} =\n{}",
            leaf.source(),
            value.source(),
            value.source(),
            indent(&body)
        ));
        Ok(name)
    }
}

impl Walker<'_> {
    /// Turn a leaf request into an exact aggregate request. The ordinary source
    /// walker then honors attached rules or follows the aggregate computation.
    /// The original leaf obligation remains in the transaction for validation.
    pub(super) fn lift_projection(
        &mut self,
        mut site: Site,
        request: &TypedValue,
        request_type: &Ty,
    ) -> Result<(Site, TypedValue, Ty), EditError> {
        let leaf_value = self.source_value(&site)?;
        let leaf = Ty::from_value(leaf_value.value_type());
        let path = std::mem::take(&mut site.projections);
        site.ty = path.first().and_then(|p| p.input.clone());
        if site.ty.is_none() {
            // Tie inference of a polymorphic aggregate to the selected field's
            // actual type without evaluating or serializing its other fields.
            let (source, helpers) = self.render_site_typed(&site, "twineLiftType")?;
            let projected = projections::render("aggregate".into(), &path, None)?;
            self.probes.declarations = helpers;
            let checked = self.probes.check(&format!(
                "leafIdentity :: {} -> {}\nleafIdentity x = x\nretain aggregate = (aggregate, leafIdentity ({projected}))\ncurrent = fst (retain (\n{}\n))\n",
                leaf.source(), leaf.source(), indent(&source),
            ))?;
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("current".into()))
                .unwrap();
            site.ty = Some(Ty::read(&mut checked.substitution.clone(), scheme.ty)?);
        }
        let aggregate = self.source_value(&site)?;
        let value = Ty::from_value(aggregate.value_type());
        let selector = self.candidate_policy(&leaf, request_type);
        self.pin_typed_policy_expression(
            &selector,
            &candidates::policy_type(&leaf, request_type, leaf.clone()),
        )?;
        let (setter, declarations) =
            compile(&mut self.probes, "", &path, &value, &leaf, "twineSourceSet")?;
        let current = self.probes.reference(&aggregate)?;
        let selected = self.probes.reference(&leaf_value)?;
        let requested = self.probes.reference(request)?;
        let output = Ty::Named("Maybe".into(), vec![value.clone()]);
        self.probes.declarations = format!(
            "{declarations}current :: {}\ncurrent = {current}\nselected :: {}\nselected = {selected}\nrequest :: {}\nrequest = {requested}\n",
            value.source(), leaf.source(), request_type.source(),
        );
        let (proposal, _) = self.probes.evaluate_lazy(&format!(
            "do\n  chosen <- ({selector}) {{ value = selected, pinned = Nothing }} request\n  candidate <- {setter} chosen current\n  pure (Just candidate)"
        ), Some(&Ty::Named("Maybe".into(), vec![output.clone()])))?;
        let request = self
            .probes
            .maybe_payload(&proposal, &output)?
            .ok_or(EditError::RuleFailed)?;
        Ok((site, request, output))
    }
}
