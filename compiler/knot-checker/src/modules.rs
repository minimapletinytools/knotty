//! Owned, in-memory module interfaces. Type variables are private to an
//! interface's arena and are always copied into the consumer's substitution.
use crate::{
    interface::instance::InstanceEntry,
    kind::Kind,
    program::TypedProgram,
    solve::{SchemeEnv, SchemeKey},
    ty::{Scheme, Structure},
    var::{Substitution, TypeVarId},
};
use knot_canonical::{
    ast::{CDecl, CType, Ref},
    env::{CtorInfo, ModuleInterface},
};
use knot_syntax::ast::decl::{Decl, ExposedItem, Exposing, Fixity, Module};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct CheckedInterface {
    pub name: Vec<String>,
    pub exports: ModuleInterface,
    pub aliases: HashMap<String, (Vec<String>, CType)>,
    pub fixities: HashMap<String, Fixity>,
    pub constructors: HashMap<String, CtorInfo>,
    pub(crate) kinds: HashMap<Ref, Kind>,
    pub(crate) instances: Vec<(String, Ref, InstanceEntry)>,
    substitution: Substitution,
    schemes: Vec<(String, Scheme)>,
}

/// Validate the export list, including unused entries, before resolving consumers.
pub fn exported_names(raw: &Module) -> Result<ModuleInterface, String> {
    let mut all = ModuleInterface::default();
    for d in &raw.decls {
        match &d.node {
            Decl::Fn(f) => {
                all.values.insert(f.name.clone());
            }
            Decl::Primitive(n, _) => {
                all.values.insert(n.clone());
            }
            Decl::TypeAlias(n, _, _) => {
                all.types.insert(n.clone());
            }
            Decl::TypeDecl(n, _, variants, _) => {
                all.types.insert(n.clone());
                all.constructors
                    .insert(n.clone(), variants.iter().map(|(n, _)| n.clone()).collect());
            }
            Decl::Interface(i) => {
                all.values.extend(i.methods.iter().map(|(n, _)| n.clone()));
            }
            _ => {}
        }
    }
    select_exports(&all, &raw.exposing)
}

/// Also used to validate an import's exposing list, even if a name is never used.
pub fn select_exports(
    all: &ModuleInterface,
    exposing: &Exposing,
) -> Result<ModuleInterface, String> {
    let Exposing::Some(items) = exposing else {
        return Ok(all.clone());
    };
    let mut result = ModuleInterface::default();
    let mut seen = HashSet::new();
    for item in items {
        let (space, name) = match item {
            ExposedItem::Value(n) => ("value", n),
            ExposedItem::TypeOnly(n) | ExposedItem::TypeWithVariants(n) => ("type", n),
        };
        if !seen.insert((space, name)) {
            return Err(format!("duplicate exposed {space} {name}"));
        }
        match item {
            ExposedItem::Value(n) if all.values.contains(n) => {
                result.values.insert(n.clone());
            }
            ExposedItem::TypeOnly(n) if all.types.contains(n) => {
                result.types.insert(n.clone());
            }
            ExposedItem::TypeWithVariants(n) if all.constructors.contains_key(n) => {
                result.types.insert(n.clone());
                result
                    .constructors
                    .insert(n.clone(), all.constructors[n].clone());
            }
            _ => {
                return Err(format!(
                    "{space} {name} is not exported (or its constructors are hidden)"
                ))
            }
        }
    }
    Ok(result)
}

fn qualify(r: &Ref, module: &[String]) -> Ref {
    match r {
        Ref::TopLevel(name) => Ref::Imported {
            module: module.to_vec(),
            name: name.clone(),
        },
        _ => r.clone(),
    }
}
fn qualify_type(ty: &CType, module: &[String]) -> CType {
    match ty {
        CType::Named(r, args) => CType::Named(
            qualify(r, module),
            args.iter().map(|t| qualify_type(t, module)).collect(),
        ),
        CType::Fn(a, b) => CType::Fn(
            Box::new(qualify_type(a, module)),
            Box::new(qualify_type(b, module)),
        ),
        CType::Tuple(ts) => CType::Tuple(ts.iter().map(|t| qualify_type(t, module)).collect()),
        CType::Record(fs, ext) => CType::Record(
            fs.iter()
                .map(|(n, t)| (n.clone(), qualify_type(t, module)))
                .collect(),
            ext.clone(),
        ),
        CType::VarApp(n, ts) => CType::VarApp(
            n.clone(),
            ts.iter().map(|t| qualify_type(t, module)).collect(),
        ),
        _ => ty.clone(),
    }
}

impl CheckedInterface {
    pub fn from_program(program: &TypedProgram, raw: &Module) -> Result<Self, String> {
        if program.canonical.name != raw.name {
            return Err("interface/module name mismatch".into());
        }
        let exports = exported_names(raw)?;
        let mut aliases = HashMap::new();
        let mut constructors = HashMap::new();
        for d in &program.canonical.decls {
            match &d.node {
                CDecl::TypeAlias(n, ps, body) if exports.types.contains(n) => {
                    aliases.insert(n.clone(), (ps.clone(), qualify_type(body, &raw.name)));
                }
                CDecl::TypeDecl(n, _, variants, _) => {
                    for (ctor, fields) in variants {
                        if exports
                            .constructors
                            .get(n)
                            .is_some_and(|cs| cs.contains(ctor))
                        {
                            constructors.insert(
                                ctor.clone(),
                                CtorInfo {
                                    arity: fields.len(),
                                    type_name: n.clone(),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        let mut names: Vec<_> = exports
            .values
            .iter()
            .chain(constructors.keys())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        let schemes = names
            .into_iter()
            .map(|name| {
                let scheme = program
                    .schemes
                    .get(&SchemeKey::TopLevel(name.clone()))
                    .ok_or_else(|| format!("missing checked scheme for {name}"))?
                    .clone();
                Ok((name, scheme))
            })
            .collect::<Result<_, String>>()?;
        let mut fixities = HashMap::new();
        for d in &raw.decls {
            if let Decl::Fixity(f) = &d.node {
                for op in &f.operators {
                    if exports.values.contains(&op.node) {
                        fixities.insert(op.node.clone(), f.fixity);
                    }
                }
            }
        }
        // Only this module's instances; the loader supplies transitive interfaces
        // once each. A diamond must neither duplicate nor hide an instance.
        let instances = program
            .instances
            .iter()
            .map(|i| {
                let head = Ref::Imported {
                    module: i.head.module.clone(),
                    name: i.head.name.clone(),
                };
                let local = if i.head.module == raw.name {
                    Ref::TopLevel(i.head.name.clone())
                } else if i.head.module == ["Prelude"] {
                    Ref::Builtin(i.head.name.clone())
                } else {
                    head.clone()
                };
                (
                    i.interface.clone(),
                    if i.head.module == ["Prelude"] {
                        local.clone()
                    } else {
                        head
                    },
                    {
                        let mut entry =
                            program.instances_table.exported_entry(&i.interface, &local);
                        if let Some(relation) = &mut entry.relation {
                            relation.targets = relation
                                .targets
                                .iter()
                                .map(|t| qualify_type(t, &raw.name))
                                .collect();
                        }
                        entry
                    },
                )
            })
            .collect();
        Ok(Self {
            name: raw.name.clone(),
            exports,
            aliases,
            fixities,
            constructors,
            instances,
            kinds: program
                .kinds
                .iter()
                .map(|(r, k)| (qualify(r, &raw.name), k.clone()))
                .collect(),
            substitution: program.substitution.clone(),
            schemes,
        })
    }

    pub(crate) fn seed(
        &self,
        target: &mut Substitution,
        env: &mut SchemeEnv,
        used: &HashSet<String>,
    ) {
        let source = &self.substitution;
        for (name, scheme) in &self.schemes {
            if !used.contains(name) {
                continue;
            }
            let mut copies = HashMap::new();
            let ty = copy_type(source, target, scheme.ty, &self.name, &mut copies);
            let vars = scheme
                .vars
                .iter()
                .map(|v| copy_type(source, target, *v, &self.name, &mut copies))
                .collect();
            let constraints = scheme
                .constraints
                .iter()
                .map(|(v, c)| {
                    (
                        copy_type(source, target, *v, &self.name, &mut copies),
                        c.clone(),
                    )
                })
                .collect();
            env.insert(
                SchemeKey::Imported(self.name.clone(), name.clone()),
                Scheme {
                    ty,
                    vars,
                    constraints,
                },
            );
        }
    }
}

fn copy_type(
    source: &Substitution,
    target: &mut Substitution,
    id: TypeVarId,
    module: &[String],
    copies: &mut HashMap<TypeVarId, TypeVarId>,
) -> TypeVarId {
    let (id, shape) = source.read_root(id);
    if let Some(copy) = copies.get(&id) {
        return *copy;
    }
    let copy = target.fresh_unbound();
    copies.insert(id, copy);
    // Rigid variables belonging to a generalized signature become the scheme's
    // fresh quantified variables, never consumer-local skolems.
    if let Some(shape) = shape {
        let mut child = |id| copy_type(source, target, id, module, copies);
        let shape = match shape {
            Structure::App(r, xs) => Structure::App(
                qualify(r, module),
                xs.iter().copied().map(&mut child).collect(),
            ),
            Structure::Ctor(r, xs) => Structure::Ctor(
                qualify(r, module),
                xs.iter().copied().map(&mut child).collect(),
            ),
            Structure::Fn(a, b) => Structure::Fn(child(*a), child(*b)),
            Structure::Predicate(xs) => {
                Structure::Predicate(xs.iter().copied().map(&mut child).collect())
            }
            Structure::Tuple(xs) => Structure::Tuple(xs.iter().copied().map(&mut child).collect()),
            Structure::Record(fs, ext) => Structure::Record(
                fs.iter().map(|(n, t)| (n.clone(), child(*t))).collect(),
                ext.map(&mut child),
            ),
            Structure::VarApp(f, xs) => {
                Structure::VarApp(child(*f), xs.iter().copied().map(&mut child).collect())
            }
            Structure::Unit => Structure::Unit,
        };
        target.bind(copy, shape);
    }
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_types_follow_aliases_and_isolate_consumer_unification() {
        let mut source = Substitution::new();
        let first = source.fresh_unbound();
        let second = source.fresh_unbound();
        let quantified = source.fresh_rigid("a".into());
        source.link_to(first, second);
        source.link_to(second, quantified);
        let function = source.fresh_bound(Structure::Fn(first, quantified));
        let before = format!("{source:?}");

        for concrete in ["Int", "String"] {
            let mut target = Substitution::new();
            let mut copies = HashMap::new();
            let copied = copy_type(&source, &mut target, function, &["Lib".into()], &mut copies);
            let Some(Structure::Fn(input, output)) = target.resolve_structure(copied) else {
                panic!("expected the imported function type");
            };
            assert_eq!(input, output, "aliases must share one quantified variable");
            assert!(target.is_unbound(input), "imported skolems become flexible");
            target.bind(input, Structure::App(Ref::Builtin(concrete.into()), vec![]));
            assert_eq!(
                target.resolve_structure(output),
                Some(Structure::App(Ref::Builtin(concrete.into()), vec![]))
            );
            assert_eq!(
                format!("{source:?}"),
                before,
                "consumer must not mutate the frozen interface"
            );
        }
    }
}
