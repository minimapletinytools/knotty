//! Map original edit sites to expressions from the checked candidate revision.
//! Spans locate an explicit source replacement, never a runtime activation.
use super::*;
use knot_canonical::ast::CModule;

pub(super) fn checked(
    patches: &[LiteralPatch],
    locations: &recipes::Locations,
    candidate: &CModule,
) -> Result<BTreeMap<(u32, u32), Spanned<CExpr>>, EditError> {
    let mut wanted = BTreeMap::new();
    let mut shift = 0i64;
    for patch in patches {
        let start = u32::try_from(i64::from(patch.span.start) + shift)
            .map_err(|_| EditError::SourceTooLarge)?;
        let end = start
            .checked_add(
                u32::try_from(patch.replacement.len()).map_err(|_| EditError::SourceTooLarge)?,
            )
            .ok_or(EditError::SourceTooLarge)?;
        let key = (patch.span.start, patch.span.end);
        let spans = locations
            .get(&key)
            .ok_or_else(|| EditError::Internal("missing source patch locations".into()))?;
        for (original, relative) in spans {
            if relative.end > end - start {
                return Err(EditError::Internal(
                    "source patch location outside replacement".into(),
                ));
            }
            wanted
                .entry((start + relative.start, start + relative.end))
                .or_insert_with(Vec::new)
                .push(*original);
        }
        shift += i64::from(end - start) - i64::from(patch.span.end - patch.span.start);
    }
    let mut result = BTreeMap::new();
    for expression in crate::edit_domain::expressions(candidate) {
        if let Some(originals) = wanted.remove(&(expression.span.start, expression.span.end)) {
            for original in originals {
                result.insert(original, expression.clone());
            }
        }
    }
    if !wanted.is_empty() {
        return Err(EditError::Internal(
            "replacement has no complete checked candidate expression".into(),
        ));
    }
    Ok(result)
}
