//! Candidate code gets fresh executable bindings but retains nominal tags only
//! after its declaration/schema boundary is checked. Existing closures keep the
//! original bindings; this is not mutation or cross-session heap transfer.
use super::*;
use knot_checker::{print::print_scheme, solve::SchemeKey};

/// Checked mapping from original value/dictionary names to one candidate's code.
/// Private construction and lineage prevent applying it to an unrelated program.
#[derive(Clone)]
pub struct RevisionOverlay {
    token: Arc<()>,
    globals: BTreeMap<Global, Global>,
}
impl RevisionOverlay {
    pub fn value(&self, name: &GlobalName) -> Option<GlobalName> {
        match self.globals.get(&Global::Value(name.clone()))? {
            Global::Value(name) => Some(name.clone()),
            _ => None,
        }
    }
}
impl LinkedProgram {
    /// Append an ABI-compatible candidate user module under fresh executable
    /// names. `original` must be the checked module previously linked here.
    pub fn candidate_revision(
        &self,
        original: &TypedProgram,
        candidate: &TypedProgram,
        namespace: Vec<String>,
    ) -> Result<(Self, ProgramExtension, RevisionOverlay), String> {
        if self.revisions.get(&original.canonical.name) != Some(&original.revision) {
            return Err("candidate needs the exact checked original module in this lineage".into());
        }
        if namespace.is_empty() || self.modules.contains(&namespace) {
            return Err("candidate executable namespace must be fresh".into());
        }
        compatible(original, candidate)?;
        let prefix = self.program.expressions.len();
        let mut program = self.program.as_ref().clone();
        lower::lower_into(&mut program, std::slice::from_ref(candidate));
        let changed = program
            .globals
            .iter()
            .filter(|(name, id)| self.program.globals.get(name) != Some(*id))
            .map(|(name, id)| (name.clone(), *id))
            .collect::<Vec<_>>();
        let mut globals = BTreeMap::new();
        let mut modules = self.modules.clone();
        modules.insert(namespace.clone());
        for (name, _) in &changed {
            let renamed = match name {
                Global::Value(name) => Global::Value(renamed(name, &namespace)),
                Global::Instance(interface, head) => {
                    Global::Instance(interface.clone(), renamed(head, &namespace))
                }
                // Structural dictionaries are compiler-generated, parameterized
                // code. An existing dictionary must never have been overwritten.
                Global::Structural(..) if !self.program.globals.contains_key(name) => continue,
                _ => return Err("candidate attempted to replace shared library code".into()),
            };
            if self.program.globals.contains_key(&renamed) {
                return Err("candidate binding collision".into());
            }
            match &renamed {
                Global::Value(name) | Global::Instance(_, name) => {
                    if self.modules.contains(&name.module) {
                        return Err("candidate namespace collision".into());
                    }
                    modules.insert(name.module.clone());
                }
                _ => unreachable!(),
            }
            globals.insert(name.clone(), renamed);
        }
        // Restore the original executable map before installing fresh names.
        for (old, id) in &changed {
            if let Some(new) = globals.get(old) {
                if let Some(original) = self.program.globals.get(old) {
                    program.globals.insert(old.clone(), *original);
                } else {
                    program.globals.remove(old);
                }
                program.globals.insert(new.clone(), *id);
            }
        }
        rewrite(&mut program, prefix, &globals);
        let program = append(program, &[], &self.hosts, prefix)?;
        let token = Arc::new(());
        let mut overlays = self.overlays.clone();
        overlays.push(token.clone());
        let next = Self {
            program: Arc::new(program),
            hosts: self.hosts.clone(),
            modules,
            revisions: self.revisions.clone(),
            overlays,
        };
        let (next, extension) = self.extension_to(next)?;
        Ok((next, extension, RevisionOverlay { token, globals }))
    }
    /// Link a checked query against the candidate's values/dictionaries. Types,
    /// constructor tags and patterns keep their original qualified identity.
    pub fn extension_for_revision(
        &self,
        modules: &[&TypedProgram],
        revision: &RevisionOverlay,
    ) -> Result<(Self, ProgramExtension), String> {
        self.extension_to(self.extend_for_revision(modules, revision)?)
    }
    // Internal preparation does not issue intermediate append capabilities;
    // the caller verifies final metadata and creates exactly one capability.
    pub(super) fn extend_for_revision(
        &self,
        modules: &[&TypedProgram],
        revision: &RevisionOverlay,
    ) -> Result<Self, String> {
        if !self
            .overlays
            .iter()
            .any(|token| Arc::ptr_eq(token, &revision.token))
        {
            return Err("candidate mapping belongs to another linked lineage".into());
        }
        let mut next = self.extend(modules)?;
        let prefix = self.program.expressions.len();
        rewrite(Arc::make_mut(&mut next.program), prefix, &revision.globals);
        Ok(next)
    }
}
fn renamed(name: &GlobalName, namespace: &[String]) -> GlobalName {
    GlobalName {
        module: namespace.iter().chain(&name.module).cloned().collect(),
        name: name.name.clone(),
    }
}
fn rewrite(program: &mut Program, prefix: usize, globals: &BTreeMap<Global, Global>) {
    for index in prefix..program.expressions.len() {
        if let Kind::Global(name) = &mut program.expressions[index].kind {
            if let Some(new) = globals.get(name) {
                *name = new.clone();
            }
        }
    }
}
fn compatible(original: &TypedProgram, candidate: &TypedProgram) -> Result<(), String> {
    if original.canonical.name != candidate.canonical.name {
        return Err("candidate module identity differs".into());
    }
    // Compare declarations without source offsets or function/method bodies.
    // Exact alias/type parameter spellings are conservative: no type migration.
    let declarations = |module: &TypedProgram| -> Result<Vec<CDecl>, String> {
        module
            .canonical
            .decls
            .iter()
            .filter_map(|d| match &d.node {
                CDecl::Fn(_) => None,
                CDecl::TypeAlias(..) | CDecl::TypeDecl(..) => Some(Ok(d.node.clone())),
                CDecl::Instance(instance) => {
                    let mut instance = instance.clone();
                    instance.methods.clear();
                    Some(Ok(CDecl::Instance(instance)))
                }
                _ => Some(Err(
                    "candidate overlays only support checked user modules".into()
                )),
            })
            .collect()
    };
    if declarations(original)? != declarations(candidate)? {
        return Err("candidate changes nominal declarations or instance contracts".into());
    }
    let schemes = |module: &TypedProgram| -> Result<BTreeMap<String, String>, String> {
        let mut sub = module.substitution.clone();
        module
            .definitions
            .iter()
            .map(|d| {
                let name = &module.bindings[&d.binding].name;
                let scheme = module
                    .schemes
                    .get(&SchemeKey::TopLevel(name.clone()))
                    .ok_or("missing candidate binding scheme")?;
                Ok((name.clone(), print_scheme(&mut sub, scheme)))
            })
            .collect()
    };
    if schemes(original)? != schemes(candidate)? {
        return Err("candidate changes value bindings or their checked types".into());
    }
    Ok(())
}
