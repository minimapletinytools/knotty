//! Explicit, one-step request translation with the original request retained as
//! a validation obligation. There is no implicit conversion search or fallback.
use super::*;

#[derive(Clone, Debug)]
pub struct RequestAdapter {
    /// A concrete Feed a -> s -> Maybe t function; Constriction s a and t a.
    pub expression: String,
}
pub(super) struct RegisteredAdapter {
    value: Ty,
    request: Ty,
    output: Ty,
    expression: String,
}
impl Walker<'_> {
    pub(super) fn register_adapters(
        &mut self,
        adapters: &[RequestAdapter],
    ) -> Result<(), EditError> {
        for adapter in adapters {
            self.register_adapter(&adapter.expression, None)?;
        }
        Ok(())
    }
    fn register_adapter(
        &mut self,
        expression: &str,
        signature: Option<&Ty>,
    ) -> Result<(), EditError> {
        self.probes.declarations.clear();
        let source = format!(
            "{}adapter =\n{}",
            signature
                .map(|t| format!("adapter :: {}\n", t.source()))
                .unwrap_or_default(),
            indent(expression)
        );
        let checked = self.probes.check(&source)?;
        let scheme = checked
            .schemes
            .get(&knot_checker::solve::SchemeKey::TopLevel("adapter".into()))
            .unwrap();
        if !scheme.vars.is_empty() || !scheme.constraints.is_empty() {
            return unsupported("request adapters must be explicitly specialized");
        }
        let Ty::Function(state, rest) = Ty::read(&mut checked.substitution.clone(), scheme.ty)?
        else {
            return unsupported("request adapter needs an input state");
        };
        let Ty::Record(fields) = &*state else {
            return unsupported("request adapter needs a value/pinned state");
        };
        let Some(value) = fields.get("value") else {
            return unsupported("request adapter needs a value field");
        };
        if *state != candidates::feed_type(value) {
            return unsupported("request adapter state types must match");
        }
        let Ty::Function(request, result) = *rest else {
            return unsupported("request adapter needs a request");
        };
        let Ty::Named(name, types) = *result else {
            return unsupported("request adapter must return Maybe constraint");
        };
        if name != "Maybe" || types.len() != 1 {
            return unsupported("request adapter must return Maybe constraint");
        }
        let output = types.into_iter().next().unwrap();
        if self
            .adapters
            .iter()
            .any(|a| &a.value == value && a.request == *request)
        {
            return unsupported("duplicate request adapter type pair");
        }
        self.probes.check(&format!("{source}validateOriginal :: {} -> {} -> Bool\nvalidateOriginal = fits\nvalidateAdapted :: {} -> {} -> Bool\nvalidateAdapted = fits\n", request.source(), value.source(), output.source(), value.source()))?;
        self.adapters.push(RegisteredAdapter {
            value: value.clone(),
            request: *request,
            output,
            expression: expression.into(),
        });
        Ok(())
    }
    pub(super) fn adapter_policy(&self, value: &Ty, request: &Ty) -> Option<(String, Ty)> {
        self.adapters
            .iter()
            .find(|a| &a.value == value && &a.request == request)
            .map(|a| (a.expression.clone(), a.output.clone()))
    }
    pub(super) fn adapt_request(
        &mut self,
        site: &Site,
        request: &TypedValue,
        request_type: &Ty,
    ) -> Result<Option<(TypedValue, Ty)>, EditError> {
        let Some(adapter) = self
            .adapters
            .iter()
            .find(|a| site.ty.as_ref() == Some(&a.value) && request_type == &a.request)
        else {
            return Ok(None);
        };
        let (output, expression) = (adapter.output.clone(), adapter.expression.clone());
        let result = self
            .run_decomposition(site, request, &output, &expression)?
            .ok_or(EditError::RuleFailed)?;
        Ok(Some((result, output)))
    }
}
