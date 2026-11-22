//! Instance materialization and default structural value updates.
use super::*;

pub(super) fn feed_type(value: &Ty) -> Ty {
    Ty::Record(BTreeMap::from([
        ("value".into(), value.clone()),
        (
            "pinned".into(),
            Ty::Named("Maybe".into(), vec![value.clone()]),
        ),
    ]))
}
impl Walker<'_> {
    pub(super) fn reject_retired_materializers(&self) -> Result<(), EditError> {
        for module in &self.snapshot.graph.modules {
            for decl in &module.program.canonical.decls {
                let retired = match &decl.node {
                    CDecl::Fn(f) => f.annotations.iter().any(|a| a.key == "materialize"),
                    CDecl::Instance(i) => i
                        .methods
                        .iter()
                        .any(|f| f.annotations.iter().any(|a| a.key == "materialize")),
                    _ => false,
                };
                if retired {
                    return unsupported("@materialize source templates were removed; implement Constriction.materialize instead");
                }
            }
        }
        Ok(())
    }
    pub(super) fn candidate_policy(&self, value: &Ty, request: &Ty) -> String {
        value_policy(value, request)
    }
    pub(super) fn choose_candidate(
        &mut self,
        site: &Site,
        request: &TypedValue,
        request_type: &Ty,
    ) -> Result<materialization::Materialized, EditError> {
        let (source, helpers) = self.render_site_typed(site, "twineCandidateType")?;
        let value_type = if let Some(ty) = &site.ty {
            ty.clone()
        } else {
            self.probes.declarations = helpers.clone();
            let checked = self
                .probes
                .check(&format!("current =\n{}", indent(&source)))?;
            let ty = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("current".into()))
                .unwrap()
                .ty;
            Ty::read(&mut checked.substitution.clone(), ty)?
        };
        let output = Ty::Named("Maybe".into(), vec![value_type.clone()]);
        // The standard policy is an ordinary generic Knot function. Its checked
        // specialization determines applicability; Rust does not recognize the
        // incoming constraint's type or constructors. No conversion is tried.
        let selector = self.candidate_policy(&value_type, request_type);
        // Called only at writable, unpinned sites. Captures/locks are rejected
        // before policy execution; final obligations independently retain pins.
        self.probes.declarations = format!(
            "{helpers}current :: {}\ncurrent =\n{}state :: {}\nstate = {{ value = current, pinned = Nothing }}\nrequest :: {}\nrequest = {}\n",
            value_type.source(), indent(&source), feed_type(&value_type).source(), request_type.source(), self.probes.reference(request)?
        );
        let (proposal, _) = self.probes.evaluate_lazy_call(
            &selector,
            &[
                ("state", feed_type(&value_type)),
                ("request", request_type.clone()),
            ],
            &output,
        )?;
        let candidate = self
            .probes
            .maybe_payload(&proposal, &value_type)?
            .ok_or(EditError::RuleFailed)?;
        self.probes
            .materialize_live(&candidate, &self.snapshot.graph.entry().program.canonical)
    }
}

/// Monomorphic policy contract used by dependency checking as well as execution.
pub(super) fn policy_type(value: &Ty, request: &Ty, output: Ty) -> Ty {
    rule_type([value.clone()], request, output)
}
pub(super) fn rule_type(values: impl IntoIterator<Item = Ty>, request: &Ty, output: Ty) -> Ty {
    let mut states = values
        .into_iter()
        .map(|v| feed_type(&v))
        .collect::<Vec<_>>();
    let state = if states.len() == 1 {
        states.pop().unwrap()
    } else {
        Ty::Tuple(states)
    };
    Ty::Function(
        Box::new(state),
        Box::new(Ty::Function(
            Box::new(request.clone()),
            Box::new(Ty::Named("Maybe".into(), vec![output])),
        )),
    )
}

// Updating an existing list preserves unconstrained slots; materializing a new
// suffix has no original slots to retain. Generated callbacks need this same
// distinction when converting their input request into an exact candidate.
fn value_policy(value: &Ty, request: &Ty) -> String {
    if let (Ty::Named(v, elements), Ty::Named(r, requests)) = (value, request) {
        if v == "List" && r == "ListConstraint" && elements.len() == 1 && requests.len() == 1 {
            let child = value_policy(&elements[0], &requests[0]);
            return format!("(\\input request -> Prelude.applyListRequest (\\value constraint -> if fits constraint value then Just value else ({child}) {{ value = value, pinned = Nothing }} constraint) input.value request)");
        }
    }
    "(\\_ request -> Prelude.materialize request)".into()
}
