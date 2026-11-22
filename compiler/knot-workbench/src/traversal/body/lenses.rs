//! Checked input updates. Constructor/pattern mismatches reject rather than
//! inventing a source location or changing a different input branch.
use super::*;

impl Compiler<'_, '_> {
    pub(super) fn projected_input(
        &mut self,
        site: &Site,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        let mut base = site.clone();
        let mut path = std::mem::take(&mut base.projections);
        let root = self.multi_root(&base);
        if let Some(index) = root {
            let inputs = self.multi.as_ref().unwrap();
            base.expr.node = CExpr::Var(Ref::TopLevel(self.symbol.clone()));
            base.ty = Some(self.input.clone());
            let prefix = base
                .projected(
                    ProjectionKind::Field("arguments".into()),
                    Some(Ty::Tuple(inputs.types.clone())),
                )
                .projected(
                    ProjectionKind::Tuple {
                        arity: inputs.types.len(),
                        index,
                    },
                    Some(inputs.types[index].clone()),
                );
            let mut projections = prefix.projections;
            projections.extend(path);
            path = projections;
        }
        if matches!(&base.expr.node, CExpr::Var(Ref::Local(_))) {
            base.ty = path.first().and_then(|p| p.input.clone());
            let mut resolved = self.walker.resolve_local(base)?;
            resolved.projections.extend(path);
            resolved.ty = site.ty.clone();
            let child = self.emit(resolved, request)?;
            return Ok(format!("{} input current request", child.unravel));
        }
        if !(base.module.is_empty()
            && matches!(&base.expr.node, CExpr::Var(Ref::TopLevel(name)) if name == &self.symbol))
        {
            let Some(first) = path.first() else {
                return unsupported("missing input projection");
            };
            base.ty = first.input.clone();
            if let ProjectionKind::Match { pattern, module } = &first.kind {
                let matched = format!("{forward}Matched");
                self.forward(&base, &matched)?;
                base.projections.extend(path[1..].iter().cloned());
                base.ty = site.ty.clone();
                let child = self.emit(base, request)?;
                return Ok(format!("do\n  (candidate, guard) <- {} input current request\n  pure (candidate, \\value -> (case {matched} value of\n    {} -> True\n    _ -> False) && guard value)", child.unravel, syntax::pattern(pattern, module)));
            }
            if let Some(mut child) = projections::direct(&base, &first.kind) {
                child.projections.extend(path[1..].iter().cloned());
                child.ty = site.ty.clone();
                let child = self.emit(child, request)?;
                return Ok(format!("{} input current request", child.unravel));
            }
            return self.computed_projection(
                base,
                &path,
                site.ty.as_ref().unwrap(),
                request,
                forward,
            );
        }
        let leaf = site.ty.as_ref().unwrap();
        let setter = self.setter(&path, &self.input.clone(), leaf, &format!("{forward}Set"))?;
        let selector = self.walker.candidate_policy(leaf, request);
        self.walker.pin_typed_policy_expression(
            &selector,
            &candidates::policy_type(leaf, request, leaf.clone()),
        )?;
        let candidate = self.mark_multi_request(root, "updated");
        Ok(format!("if fits request ({forward} current) then Just (current, \\value -> fits request ({forward} value)) else\n  do\n    chosen <- ({selector}) {} request\n    updated <- {setter} chosen current\n    let candidate = {candidate}\n    pure (candidate, \\value -> fits request ({forward} value))", self.state(forward)))
    }
    /// Lift a selected leaf candidate into the original aggregate, then route an
    /// ordinary exact request through its source computation and selected rules.
    /// Unselected fields retain their original values; the checker requires Eq
    /// for the aggregate rather than inventing wildcard constraints for them.
    fn computed_projection(
        &mut self,
        mut base: Site,
        path: &[projections::Projection],
        leaf: &Ty,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        let original = format!("{forward}Aggregate");
        let (value, _) = self.forward(&base, &original)?;
        base.ty = Some(value.clone());
        let setter = self.setter(path, &value, leaf, &format!("{forward}AggregateSet"))?;
        let selector = self.walker.candidate_policy(leaf, request);
        self.walker.pin_typed_policy_expression(
            &selector,
            &candidates::policy_type(leaf, request, leaf.clone()),
        )?;
        let child = self.emit(base, &optional(value))?;
        Ok(format!("do\n  chosen <- ({selector}) {} request\n  aggregate <- {setter} chosen ({original} {})\n  (candidate, guard) <- {} input current (Just aggregate)\n  pure (candidate, \\value -> guard value && fits request ({forward} value))", self.state(forward), self.current_value(), child.unravel))
    }
    fn setter(
        &mut self,
        path: &[projections::Projection],
        value: &Ty,
        leaf: &Ty,
        prefix: &str,
    ) -> Result<String, EditError> {
        let ambient = self.metadata();
        let (name, declarations) = projection_updates::compile(
            &mut self.walker.probes,
            &ambient,
            path,
            value,
            leaf,
            prefix,
        )?;
        self.declarations.push_str(&declarations);
        Ok(name)
    }
}
