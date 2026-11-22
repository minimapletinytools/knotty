//! Optional demand is an explicit request wrapper for any unravel input. It is
//! never inferred from the meaning/constructors of an arbitrary sensitivity.
use super::*;

pub(super) fn payload(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::Named(name, args) if name == "FieldRequest" && args.len() == 1 => Some(&args[0]),
        _ => None,
    }
}
impl Walker<'_> {
    pub(super) fn run_field(
        &mut self,
        site: Site,
        request: TypedValue,
        ty: Ty,
    ) -> Result<(), EditError> {
        if let Some(inner) = payload(&ty) {
            let (constructor, arity) = self.probes.constructor_live(&request)?;
            if constructor.module == ["Prelude"] && constructor.name == "NoRequest" && arity == 0 {
                // Check the relation even for an unconstrained request. The
                // standard instance does not demand the source value.
                if !self.fits(&site, &request, &ty)? {
                    return Err(EditError::ValidationFailed);
                }
                return Ok(());
            }
            if constructor.module != ["Prelude"] || constructor.name != "RequestField" || arity != 1
            {
                return Err(EditError::Internal("invalid field request envelope".into()));
            }
            self.probes.declarations.clear();
            let request = self.probes.call_typed(
                    "\\envelope -> case envelope of\n  RequestField request -> request\n  NoRequest -> error \"missing field request\"",
                    &[request],
                    inner,
                )?;
            return self.schedule(site, request, inner.clone());
        }
        self.walk_one(site, request, ty)
    }
}
