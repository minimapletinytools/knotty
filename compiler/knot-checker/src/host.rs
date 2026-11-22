//! Monomorphic declarations supplied by an embedding application. Native types
//! are nominal, constructorless and kind Type; no instances are invented.
use crate::{
    program::GlobalName,
    solve::{SchemeEnv, SchemeKey},
    ty::{Scheme, Structure},
    var::{Substitution, TypeVarId},
};
use knot_canonical::{
    ast::Ref,
    env::{ModuleInterface, ModuleRegistry},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum HostType {
    Int,
    Integer,
    Float,
    String,
    Bool,
    Unit,
    Native(GlobalName),
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HostSignature {
    pub parameters: Vec<HostType>,
    pub result: HostType,
}
#[derive(Debug, Clone, Default)]
pub struct HostDeclarations {
    pub types: BTreeSet<GlobalName>,
    pub functions: BTreeMap<GlobalName, HostSignature>,
}
impl HostType {
    fn bind(&self, sub: &mut Substitution) -> TypeVarId {
        let reference = match self {
            Self::Unit => return sub.fresh_bound(Structure::Unit),
            Self::Native(n) => Ref::Imported {
                module: n.module.clone(),
                name: n.name.clone(),
            },
            _ => Ref::Builtin(
                match self {
                    Self::Int => "Int",
                    Self::Integer => "Integer",
                    Self::Float => "Float",
                    Self::String => "String",
                    Self::Bool => "Bool",
                    _ => unreachable!(),
                }
                .into(),
            ),
        };
        sub.fresh_bound(Structure::App(reference, vec![]))
    }
}
impl HostDeclarations {
    pub(crate) fn seed(&self, sub: &mut Substitution, env: &mut SchemeEnv) {
        for (name, sig) in &self.functions {
            let mut ty = sig.result.bind(sub);
            for arg in sig.parameters.iter().rev() {
                let a = arg.bind(sub);
                ty = sub.fresh_bound(Structure::Fn(a, ty));
            }
            env.insert(
                SchemeKey::Imported(name.module.clone(), name.name.clone()),
                Scheme::monomorphic(ty),
            );
        }
    }
}
impl ModuleRegistry for HostDeclarations {
    fn exports(&self, module: &[String]) -> Option<ModuleInterface> {
        let types = self
            .types
            .iter()
            .filter(|n| n.module == module)
            .map(|n| n.name.clone())
            .collect();
        let values = self
            .functions
            .keys()
            .filter(|n| n.module == module)
            .map(|n| n.name.clone())
            .collect();
        let iface = ModuleInterface {
            types,
            values,
            constructors: Default::default(),
        };
        if iface.types.is_empty() && iface.values.is_empty() {
            None
        } else {
            Some(iface)
        }
    }
}
