//! Product request policies are checked Knot functions. Source shape determines
//! only field identities; it does not determine a custom request's meaning.
use super::*;

/// Ordinary per-field unravel rule for a concrete tuple or record type.
/// One field takes one Feed; multiple fields take a tuple of Feeds. Record
/// fields use alphabetical label order. Results use the same grouping.
#[derive(Clone, Debug)]
pub struct ProductRule {
    pub value_type: String,
    pub expression: String,
}
#[derive(Clone)]
pub(super) struct RegisteredProduct {
    expression: String,
    value: Ty,
}
fn same_shape(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Tuple(a), Ty::Tuple(b)) => a.len() == b.len(),
        (Ty::Record(a), Ty::Record(b)) => a.keys().eq(b.keys()),
        _ => false,
    }
}
impl Walker<'_> {
    pub(super) fn register_products(&mut self, rules: &[ProductRule]) -> Result<(), EditError> {
        for rule in rules {
            self.probes.declarations.clear();
            let source = format!(
                "product :: {}\nproduct = undefined\nrule0 =\n{}",
                rule.value_type,
                indent(&rule.expression)
            );
            let checked = self.probes.check(&source)?;
            let ty = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("product".into()))
                .unwrap()
                .ty;
            let value = Ty::read(&mut checked.substitution.clone(), ty)?;
            let fields = match &value {
                Ty::Tuple(fields) => fields.clone(),
                Ty::Record(fields) => fields.values().cloned().collect(),
                _ => return unsupported("product rule requires a tuple or record type"),
            };
            if fields.is_empty() {
                return unsupported("empty products have no editable fields");
            }
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("rule0".into()))
                .unwrap();
            if !scheme.vars.is_empty() || !scheme.constraints.is_empty() {
                return unsupported("product rules must specialize concretely");
            }
            let Ty::Function(_, rest) = Ty::read(&mut checked.substitution.clone(), scheme.ty)?
            else {
                return unsupported("product rule needs field feeds");
            };
            let Ty::Function(request, _) = *rest else {
                return unsupported("product rule needs an output request");
            };
            let input = if fields.len() == 1 {
                fields[0].clone()
            } else {
                Ty::Tuple(fields.clone())
            };
            let source = format!("{source}frame :: ({}, {})\nframe = undefined\nrequest :: {}\nrequest = undefined\n", value.source(), input.source(), request.source());
            let checked = self.probes.check(&source)?;
            call_frame::CallPlan::new(checked, fields.len())?
                .try_select(1)?
                .ok_or(EditError::RuleFailed)?;
            self.products.push(RegisteredProduct {
                expression: rule.expression.clone(),
                value,
            });
        }
        Ok(())
    }
    pub(super) fn product_requests(
        &mut self,
        site: &Site,
        request: TypedValue,
        request_ty: Ty,
    ) -> Result<(TypedValue, Ty), EditError> {
        let value = site.ty.as_ref().ok_or_else(|| {
            EditError::Unsupported("product decomposition requires a checked value type".into())
        })?;
        let children = match (&site.expr.node, value) {
            (CExpr::Tuple(expressions), Ty::Tuple(types)) => expressions
                .iter()
                .zip(types)
                .map(|(e, t)| Site {
                    ty: Some(t.clone()),
                    ..site.child(e)
                })
                .collect::<Vec<_>>(),
            (CExpr::Record(expressions), Ty::Record(types)) => expressions
                .iter()
                .map(|(n, e)| Site {
                    ty: types.get(n).cloned(),
                    ..site.child(e)
                })
                .collect(),
            (CExpr::RecordUpdate(_, _), Ty::Record(types)) => types
                .iter()
                .map(|(n, t)| site.projected(ProjectionKind::Field(n.clone()), Some(t.clone())))
                .collect(),
            _ => return unsupported("product source and checked shape disagree"),
        };
        self.remember_product_fields(site, &children)?;
        self.retain_argument_pins(&children)?;
        let (expression, fields) = self.product_policy(value, &request_ty)?;
        let Some(expression) = expression else {
            return Ok((request, request_ty));
        };
        let proposal = self.run_decomposition(site, &request, &fields, &expression)?;
        Ok((proposal.ok_or(EditError::RuleFailed)?, fields))
    }
    pub(super) fn product_policy(
        &self,
        value: &Ty,
        _request: &Ty,
    ) -> Result<(Option<String>, Ty), EditError> {
        if matches!(value, Ty::Record(fields) if fields.is_empty()) {
            return Ok((
                Some("\\_ request -> if Prelude.fits request {} then Just {} else Nothing".into()),
                value.clone(),
            ));
        }
        Err(EditError::Internal(
            "product default bypassed shared rule dispatch".into(),
        ))
    }
    pub(super) fn run_decomposition(
        &mut self,
        site: &Site,
        request: &TypedValue,
        fields: &Ty,
        expression: &str,
    ) -> Result<Option<TypedValue>, EditError> {
        let current = self.source_value(site)?;
        // Policy code is closed over module globals. Pass actual lazy values to
        // one checked specialization instead of rebuilding declarations per call.
        let function = format!(
            "\\current request -> (\n{}\n) {{ value = current, pinned = {} }} request",
            indent(expression),
            if self.is_pinned(site, 0) {
                "Just current"
            } else {
                "Nothing"
            },
        );
        self.probes.declarations.clear();
        let proposal = self.probes.call_typed(
            &function,
            &[current, request.clone()],
            &Ty::Named("Maybe".into(), vec![fields.clone()]),
        )?;
        self.probes.maybe_payload(&proposal, fields)
    }
}

impl Walker<'_> {
    /// Generate a normal per-field rule from checked product shape. Record
    /// fields keep source order, including when labels are not alphabetical.
    pub(super) fn default_product_rule(
        &mut self,
        site: &Site,
        request: &Ty,
    ) -> Result<Option<(Vec<Site>, String, Ty)>, EditError> {
        let value = site
            .ty
            .as_ref()
            .ok_or_else(|| EditError::Internal("product type".into()))?;
        let (children, labels) = match (&site.expr.node, value) {
            (CExpr::Tuple(expressions), Ty::Tuple(types)) => (
                expressions
                    .iter()
                    .zip(types)
                    .map(|(e, t)| Site {
                        ty: Some(t.clone()),
                        ..site.child(e)
                    })
                    .collect::<Vec<_>>(),
                None,
            ),
            (CExpr::Record(expressions), Ty::Record(types)) => (
                expressions
                    .iter()
                    .map(|(name, e)| Site {
                        ty: Some(types[name].clone()),
                        ..site.child(e)
                    })
                    .collect(),
                Some(
                    expressions
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>(),
                ),
            ),
            (CExpr::RecordUpdate(base, expressions), Ty::Record(types)) => (
                types
                    .iter()
                    .map(|(name, ty)| {
                        let mut child = expressions
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, e)| site.child(e))
                            .unwrap_or_else(|| {
                                site.child(base).projected(
                                    ProjectionKind::Field(name.clone()),
                                    Some(ty.clone()),
                                )
                            });
                        child.ty = Some(ty.clone());
                        child
                    })
                    .collect(),
                Some(types.keys().cloned().collect()),
            ),
            _ => return unsupported("product source and checked shape disagree"),
        };
        // Empty products have no editable arguments.
        if children.is_empty() {
            return Ok(None);
        }
        let names = (0..children.len())
            .map(|i| format!("field{i}"))
            .collect::<Vec<_>>();
        let rules = self
            .products
            .iter()
            .filter(|p| &p.value == value)
            .map(|p| p.expression.clone())
            .collect::<Vec<_>>();
        if !rules.is_empty() {
            let mut ordered = children.clone();
            if let Some(labels) = &labels {
                let mut positions = (0..labels.len()).collect::<Vec<_>>();
                positions.sort_by_key(|&i| &labels[i]);
                ordered = positions.into_iter().map(|i| children[i].clone()).collect();
            }
            if let Some((rule, input)) = self.select_field_rule(site, &ordered, request, &rules)? {
                return Ok(Some((ordered, rule, input)));
            }
        }
        let exact = request == &Ty::Named("Maybe".into(), vec![value.clone()]);
        if !exact && !same_shape(value, request) {
            return unsupported("product request needs a matching shape or an explicit rule");
        }
        let (parts, projection) = if let Some(labels) = labels {
            let parts = labels
                .iter()
                .map(|name| format!("candidate.{name}"))
                .collect::<Vec<_>>();
            (parts, String::new())
        } else {
            (
                names.clone(),
                format!("case candidate of\n  {} -> ", group(&names)),
            )
        };
        let body = if exact {
            let requested = parts
                .iter()
                .map(|part| format!("Just ({part})"))
                .collect::<Vec<_>>();
            let skipped = vec!["Nothing".into(); children.len()];
            format!(
                "case request of\n  Nothing -> Just ({})\n  Just candidate ->\n{}",
                group(&skipped),
                indent(&indent(&format!(
                    "{projection}Just ({})",
                    group(&requested)
                )))
            )
        } else {
            format!(
                "let candidate = request in\n{}",
                indent(&format!("{projection}Just ({})", group(&parts)))
            )
        };
        let expression = format!("\\_ request ->\n{}", indent(&body));
        let (expression, input) = self
            .select_field_rule(site, &children, request, &[expression])?
            .ok_or_else(|| {
                EditError::Check(
                    "generated product rule requires compatible field validators".into(),
                )
            })?;
        Ok(Some((children, expression, input)))
    }
}
