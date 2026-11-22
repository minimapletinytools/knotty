//! Fixed metadata-key types. Unravel contracts are checked by `annotation::unravel`.
//! There is no structural-Sensitivity signature derivation; general metadata value
//! checking remains deferred.
use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};
use knot_canonical::ast::Ref;

fn app0(sub: &mut Substitution, name: &str) -> TypeVarId {
    sub.fresh_bound(Structure::App(Ref::Builtin(name.to_string()), vec![]))
}

/// The fixed-type keys (spec §13.4) — everything except `unravel`. `None`
/// for `unravel` itself and for any unknown
/// key (the annotation set is open, spec §13.4 — an unrecognized key isn't
/// an error at this layer, just something this table has nothing to say
/// about).
pub fn fixed_expected_type(sub: &mut Substitution, key: &str) -> Option<TypeVarId> {
    match key {
        "nodeId" | "label" | "doc" | "color" | "group" => Some(app0(sub, "String")),
        "position" => {
            let x = app0(sub, "Float");
            let y = app0(sub, "Float");
            Some(sub.fresh_bound(Structure::Tuple(vec![x, y])))
        }
        "collapsed" => Some(app0(sub, "Bool")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_keys_get_their_documented_types() {
        let mut sub = Substitution::new();
        let node_id_ty = fixed_expected_type(&mut sub, "nodeId").unwrap();
        assert_eq!(
            sub.resolve_structure(node_id_ty),
            Some(Structure::App(Ref::Builtin("String".to_string()), vec![]))
        );
        let collapsed_ty = fixed_expected_type(&mut sub, "collapsed").unwrap();
        assert_eq!(
            sub.resolve_structure(collapsed_ty),
            Some(Structure::App(Ref::Builtin("Bool".to_string()), vec![]))
        );
        let position_ty = fixed_expected_type(&mut sub, "position").unwrap();
        match sub.resolve_structure(position_ty) {
            Some(Structure::Tuple(elems)) => assert_eq!(elems.len(), 2),
            other => panic!("expected a 2-tuple, got {other:?}"),
        }
    }

    #[test]
    fn unravel_is_not_a_fixed_key() {
        let mut sub = Substitution::new();
        assert!(fixed_expected_type(&mut sub, "unravel").is_none());
    }

    #[test]
    fn unknown_key_has_no_expected_type() {
        let mut sub = Substitution::new();
        assert!(fixed_expected_type(&mut sub, "bogus").is_none());
    }
}
