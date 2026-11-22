//! Checked linking for a complete executable graph. Lowering alone deliberately
//! remains useful for standalone typed fragments and inspector tooling.
use crate::*;
use knot_canonical::ast::CDecl;
use knot_checker::program::{ModuleRevisionId, TypedProgram};
mod factory;
mod revision;
pub use revision::RevisionOverlay;
use std::{collections::BTreeSet, sync::Arc};

/// An immutable, fully validated executable prefix. Only checked append operations
/// preserve this proof; obtaining a mutable Program consumes it.
#[derive(Clone)]
pub struct LinkedProgram {
    program: Arc<Program>,
    hosts: HostDeclarations,
    modules: BTreeSet<Vec<String>>,
    revisions: BTreeMap<Vec<String>, ModuleRevisionId>,
    overlays: Vec<Arc<()>>,
}
impl LinkedProgram {
    pub fn new(program: Program, hosts: &HostDeclarations) -> Result<Self, String> {
        let program = link_borrowed(program, &[], hosts)?;
        let modules = program
            .globals
            .keys()
            .filter_map(|global| match global {
                Global::Value(name) => Some(name.module.clone()),
                _ => None,
            })
            .collect();
        Ok(Self {
            program: Arc::new(program),
            hosts: hosts.clone(),
            modules,
            revisions: BTreeMap::new(),
            overlays: vec![],
        })
    }
    pub fn program(&self) -> &Program {
        &self.program
    }
    pub fn extend(&self, modules: &[&TypedProgram]) -> Result<Self, String> {
        let mut linked_modules = self.modules.clone();
        let mut revisions = self.revisions.clone();
        for module in modules {
            if !linked_modules.insert(module.canonical.name.clone()) {
                return Err("duplicate module/type namespace in checked linking".into());
            }
            revisions.insert(module.canonical.name.clone(), module.revision);
        }
        Ok(Self {
            program: Arc::new(append(
                self.program.as_ref().clone(),
                modules,
                &self.hosts,
                self.program.expressions.len(),
            )?),
            hosts: self.hosts.clone(),
            modules: linked_modules,
            revisions,
            overlays: self.overlays.clone(),
        })
    }
    pub fn shared_program(&self) -> Arc<Program> {
        self.program.clone()
    }
    /// An unforgeable append-only capability for the exact immutable base.
    pub fn extension(&self, modules: &[&TypedProgram]) -> Result<(Self, ProgramExtension), String> {
        let next = self.extend(modules)?;
        self.extension_to(next)
    }
    fn extension_to(&self, next: Self) -> Result<(Self, ProgramExtension), String> {
        let added = next
            .program
            .globals
            .additions_since(&self.program.globals)
            .ok_or("checked extension changed or removed prefix metadata")?;
        if !next
            .program
            .local_names
            .preserves(&self.program.local_names)
        {
            return Err("checked extension changed or removed prefix metadata".into());
        }
        let extension = ProgramExtension {
            base: self.program.clone(),
            program: next.program.clone(),
            added,
        };
        Ok((next, extension))
    }
    pub fn into_program(self) -> Program {
        Arc::unwrap_or_clone(self.program)
    }
}

/// Constructed only by checked append linking. Mutable program extraction or
/// an equivalent-looking separately linked base cannot forge this lineage.
#[derive(Clone)]
pub struct ProgramExtension {
    base: Arc<Program>,
    program: Arc<Program>,
    added: Vec<(Global, ExprId)>,
}
impl ProgramExtension {
    pub fn accepts(&self, base: &Arc<Program>) -> bool {
        Arc::ptr_eq(&self.base, base)
    }
    pub fn program(&self) -> Arc<Program> {
        self.program.clone()
    }
    /// Exact new bindings computed while verifying that prefix metadata is
    /// unchanged. Runtime installation need not rediscover them by scanning.
    pub fn added_globals(&self) -> &[(Global, ExprId)] {
        &self.added
    }
}

pub fn link<T: std::borrow::Borrow<TypedProgram>>(
    libraries: Program,
    modules: &[T],
    hosts: &HostDeclarations,
) -> Result<Program, String> {
    link_borrowed(
        libraries,
        &modules.iter().map(|m| m.borrow()).collect::<Vec<_>>(),
        hosts,
    )
}

/// Link without consuming the checked graph retained by an editor snapshot.
pub fn link_borrowed(
    libraries: Program,
    modules: &[&TypedProgram],
    hosts: &HostDeclarations,
) -> Result<Program, String> {
    append(libraries, modules, hosts, 0)
}
fn append(
    mut libraries: Program,
    modules: &[&TypedProgram],
    hosts: &HostDeclarations,
    checked_prefix: usize,
) -> Result<Program, String> {
    let mut names = BTreeSet::new();
    for module in modules {
        let mut declared = Vec::new();
        for definition in &module.definitions {
            declared.push(Global::Value(
                module.bindings[&definition.binding]
                    .global
                    .clone()
                    .expect("top-level definition"),
            ));
        }
        for default in &module.defaults {
            declared.push(Global::Default(
                default.interface.clone(),
                module.bindings[&default.definition.binding].name.clone(),
            ));
        }
        for instance in &module.instances {
            declared.push(Global::Instance(
                instance.interface.clone(),
                instance.head.clone(),
            ));
        }
        for decl in &module.canonical.decls {
            let values = match &decl.node {
                CDecl::Primitive(n, _) => vec![n],
                CDecl::TypeDecl(_, _, ctors, _) => ctors.iter().map(|(n, _)| n).collect(),
                _ => vec![],
            };
            for name in values {
                declared.push(Global::Value(GlobalName {
                    module: module.canonical.name.clone(),
                    name: name.clone(),
                }));
            }
        }
        for name in declared {
            if libraries.globals.contains_key(&name) || !names.insert(name.clone()) {
                return Err(format!("duplicate linked global {name:?}"));
            }
        }
    }
    for module in modules {
        lower::lower_into(&mut libraries, std::slice::from_ref(*module));
    }
    for index in checked_prefix..libraries.expressions.len() {
        let expr = &libraries.expressions[index];
        match &expr.kind {
            Kind::Global(Global::Structural(..)) => {} // Runtime-generated structural dictionary.
            Kind::Global(name) if !libraries.globals.contains_key(name) => {
                return Err(format!(
                    "unresolved linked global {name:?} at {:?}",
                    expr.origin
                ))
            }
            Kind::Host(name, signature) if hosts.functions.get(name) != Some(signature) => {
                return Err(format!("host signature mismatch for {name:?}"))
            }
            _ => {}
        }
    }
    Ok(libraries)
}

#[cfg(test)]
mod append_tests {
    use super::*;
    #[test]
    fn capability_includes_new_names_even_when_they_reference_existing_code() {
        let checked = knot_checker::check::check_library_program("Prelude").unwrap();
        let base = LinkedProgram::new(crate::lower(&[checked]), &Default::default()).unwrap();
        let name = Global::Value(GlobalName {
            module: vec!["Alias".into()],
            name: "existing".into(),
        });
        let target = *base.program.globals.values().next().unwrap();
        let mut next = base.clone();
        Arc::make_mut(&mut next.program)
            .globals
            .insert(name.clone(), target);
        let (_, capability) = base.extension_to(next).unwrap();
        assert_eq!(capability.added_globals(), [(name, target)]);
        let mut invalid = base.clone();
        let first = invalid.program.globals.keys().next().unwrap().clone();
        Arc::make_mut(&mut invalid.program).globals.remove(&first);
        assert!(base.extension_to(invalid).is_err());
        let mut invalid = base.clone();
        let local = *invalid.program.local_names.keys().next().unwrap();
        Arc::make_mut(&mut invalid.program)
            .local_names
            .remove(&local);
        assert!(base.extension_to(invalid).is_err());
        let mut invalid = base.clone();
        Arc::make_mut(&mut invalid.program)
            .local_names
            .insert(local, "changed capture name".into());
        assert!(base.extension_to(invalid).is_err());
    }
}
