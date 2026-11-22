//! Reuse the source walker's checked field policies in generated Knot helpers.
use super::*;

impl Compiler<'_, '_> {
    pub(super) fn translated(
        &mut self,
        site: &Site,
        forward: &str,
        expression: String,
        request: &Ty,
        output: Ty,
    ) -> Result<String, EditError> {
        self.walker.pin_typed_policy_expression(
            &expression,
            &candidates::policy_type(site.ty.as_ref().unwrap(), request, output.clone()),
        )?;
        let child = self.emit(site.clone(), &output)?;
        Ok(format!("do\n  translated <- ({expression}) {} request\n  (candidate, guard) <- {} input current translated\n  pure (candidate, \\value -> guard value && fits request ({forward} value))", self.state(forward), child.unravel))
    }
    pub(super) fn state(&self, forward: &str) -> String {
        format!(
            "{{ value = {forward} {}, pinned = ({}) }}",
            self.current_value(),
            self.input_pin(forward)
        )
    }
    pub(super) fn product(
        &mut self,
        site: &Site,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        if let Some((children, rule, input)) = self.walker.default_product_rule(site, request)? {
            return self.unravel_apply(&rule, children, input, forward);
        }
        let value = site.ty.as_ref().unwrap();
        let (policy, fields) = self.walker.product_policy(value, request)?;
        let policy =
            policy.map(|expression| (expression, policy_type(value, request, fields.clone())));
        let (children, requests, binding, references) = match (&site.expr.node, value, fields) {
            (CExpr::Tuple(expressions), Ty::Tuple(types), Ty::Tuple(requests)) => {
                let children = expressions
                    .iter()
                    .zip(types)
                    .map(|(e, t)| Site {
                        ty: Some(t.clone()),
                        ..site.child(e)
                    })
                    .collect::<Vec<_>>();
                let names = (0..children.len())
                    .map(|i| format!("part{i}"))
                    .collect::<Vec<_>>();
                (children, requests, group(&names), names)
            }
            (
                CExpr::Record(_) | CExpr::RecordUpdate(_, _),
                Ty::Record(types),
                Ty::Record(requests),
            ) => {
                let children = types
                    .iter()
                    .map(|(name, ty)| {
                        let mut child = match &site.expr.node {
                            CExpr::Record(fields) => {
                                site.child(&fields.iter().find(|(n, _)| n == name).unwrap().1)
                            }
                            CExpr::RecordUpdate(base, fields) => fields
                                .iter()
                                .find(|(n, _)| n == name)
                                .map(|(_, e)| site.child(e))
                                .unwrap_or_else(|| {
                                    site.child(base).projected(
                                        ProjectionKind::Field(name.clone()),
                                        Some(ty.clone()),
                                    )
                                }),
                            _ => unreachable!(),
                        };
                        child.ty = Some(ty.clone());
                        child
                    })
                    .collect::<Vec<_>>();
                let refs = types.keys().map(|n| format!("parts.{n}")).collect();
                (
                    children,
                    requests.into_values().collect(),
                    "parts".into(),
                    refs,
                )
            }
            _ => return unsupported("generated product request shape mismatch"),
        };
        self.fields(children, requests, &binding, references, policy, forward)
    }
    pub(super) fn constructed(
        &mut self,
        site: &Site,
        constructor: &GlobalName,
        children: Vec<Site>,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        if let Some((rule, input)) =
            self.walker
                .select_constructor_rule(site, constructor, &children, request)?
        {
            self.walker.pin_typed_policy_expression(
                &rule,
                &candidates::rule_type(
                    children.iter().map(|child| child.ty.clone().unwrap()),
                    request,
                    input.clone(),
                ),
            )?;
            return self.unravel_apply(&rule, children, input, forward);
        }
        let value = site.ty.as_ref().unwrap();
        let types = children
            .iter()
            .map(|c| {
                c.ty.clone().ok_or_else(|| {
                    EditError::Unsupported("generated constructor needs concrete fields".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some((rule, input)) =
            self.walker
                .select_list_rule(site, constructor, &children, request)?
        {
            return self.unravel_apply(&rule, children, input, forward);
        }
        if request != &optional(value.clone()) {
            return unsupported("generated constructor needs a registered unravel rule");
        }
        if children.is_empty() {
            return Ok(format!("if fits request ({forward} current) then Just (current, \\value -> fits request ({forward} value)) else Nothing"));
        }
        let rule = constructor_rules::exact_constructor_rule(constructor, &types);
        let (rule, input) = self
            .walker
            .select_field_rule(site, &children, request, &[rule])?
            .ok_or(EditError::RuleFailed)?;
        self.unravel_apply(&rule, children, input, forward)
    }
    #[allow(clippy::too_many_arguments)]
    fn fields(
        &mut self,
        children: Vec<Site>,
        requests: Vec<Ty>,
        binding: &str,
        references: Vec<String>,
        policy: Option<(String, Ty)>,
        forward: &str,
    ) -> Result<String, EditError> {
        if children.len() != requests.len() || children.len() != references.len() {
            return Err(EditError::Internal("generated field request arity".into()));
        }
        let proposal = if let Some((policy, ty)) = policy {
            self.walker.pin_typed_policy_expression(&policy, &ty)?;
            format!("({policy}) {} request", self.state(forward))
        } else {
            "Just request".into()
        };
        let mut nodes = Vec::new();
        for (child, request) in children.into_iter().zip(requests) {
            nodes.push(self.emit(child, &request)?);
        }
        let mut body = format!("do\n  {binding} <- {proposal}\n");
        for (i, (child, request)) in nodes.iter().zip(references).enumerate() {
            let previous = if i == 0 {
                "current".into()
            } else {
                format!("candidate{}", i - 1)
            };
            body.push_str(&format!(
                "  (candidate{i}, guard{i}) <- {} input {previous} ({request})\n",
                child.unravel
            ));
        }
        let candidate = if nodes.is_empty() {
            "current".into()
        } else {
            format!("candidate{}", nodes.len() - 1)
        };
        let guards = (0..nodes.len())
            .map(|i| format!("guard{i} value && "))
            .collect::<String>();
        body.push_str(&format!(
            "  pure ({candidate}, \\value -> {guards}fits request ({forward} value))"
        ));
        Ok(body)
    }
    pub(super) fn field(&mut self, site: Site, request: &Ty) -> Result<Node, EditError> {
        if let Some(inner) = field_requests::payload(request) {
            let child = self.emit(site, inner)?;
            let name = format!("{}Demand", child.unravel);
            let result = Ty::Tuple(vec![
                self.input.clone(),
                Ty::Function(
                    Box::new(self.input.clone()),
                    Box::new(Ty::Named("Bool".into(), vec![])),
                ),
            ]);
            self.declarations.push_str(&format!("{name} :: {} -> {} -> {} -> {}\n{name} input current request = case request of\n  NoRequest -> Just (current, \\_ -> True)\n  RequestField constraint -> {} input current constraint\n", candidates::feed_type(&self.input).source(), self.input.source(), request.source(), optional(result).source(), child.unravel));
            return Ok(Node {
                unravel: name,
                ..child
            });
        }
        self.emit(site, request)
    }
}

fn policy_type(value: &Ty, request: &Ty, fields: Ty) -> Ty {
    Ty::Function(
        Box::new(candidates::feed_type(value)),
        Box::new(Ty::Function(
            Box::new(request.clone()),
            Box::new(optional(fields)),
        )),
    )
}
