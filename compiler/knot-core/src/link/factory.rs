//! Close a checked query module over explicit monomorphic input bindings. Its
//! helpers become ordinary recursive let bindings inside a callable, so each
//! invocation owns the right captures and shares its own lazy helper results.
use super::*;
use std::collections::BTreeMap;

impl LinkedProgram {
    pub fn parameterized_extension(
        &self,
        module: &TypedProgram,
        inputs: &[&str],
        output: &str,
        revision: Option<&RevisionOverlay>,
    ) -> Result<(Self, ProgramExtension, GlobalName), String> {
        if module
            .canonical
            .decls
            .iter()
            .any(|d| !matches!(d.node, CDecl::Fn(_)))
        {
            return Err("parameterized operations require a value-only checked module".into());
        }
        let mut unique = BTreeSet::new();
        for input in inputs {
            if !unique.insert(*input) {
                return Err("duplicate operation input".into());
            }
        }
        let definition = module
            .definition(output)
            .ok_or("missing operation output")?;
        let origin = Origin {
            node: module.node_ref(definition.body.id),
            span: module.nodes[&definition.body.id].span,
            role: Role::Synthetic,
        };
        let mut next = if let Some(revision) = revision {
            self.extend_for_revision(&[module], revision)?
        } else {
            self.extend(&[module])?
        };
        let program = Arc::make_mut(&mut next.program);
        let prefix = self.program.expressions.len();
        let mut locals = BTreeMap::new();
        let mut expressions = BTreeMap::new();
        for definition in &module.definitions {
            let binding = &module.bindings[&definition.binding];
            let name = Global::Value(
                binding
                    .global
                    .clone()
                    .ok_or("operation helper is not global")?,
            );
            let expression = program
                .globals
                .remove(&name)
                .ok_or("missing operation helper code")?;
            let local = LocalId(program.next_local);
            program.next_local += 1;
            program.local_names.insert(local, binding.name.clone());
            expressions.insert(binding.name.clone(), (local, expression));
            locals.insert(name, local);
        }
        let parameters = inputs
            .iter()
            .map(|name| {
                expressions
                    .get(*name)
                    .map(|(local, _)| *local)
                    .ok_or_else(|| format!("missing operation input {name}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output_local = expressions
            .get(output)
            .ok_or("missing operation output code")?
            .0;
        for index in prefix..program.expressions.len() {
            if let Kind::Global(name) = &program.expressions[index].kind {
                if let Some(local) = locals.get(name) {
                    program.expressions[index].kind = Kind::Local(*local);
                }
            }
        }
        let bindings = expressions
            .into_iter()
            .filter(|(name, _)| !unique.contains(name.as_str()))
            .map(|(_, pair)| pair)
            .collect();
        let body = program.push(origin, Kind::Local(output_local));
        let mut body = program.push(origin, Kind::Let(bindings, body));
        for parameter in parameters.into_iter().rev() {
            body = program.push(origin, Kind::Lam(parameter, body));
        }
        // The original module's output has a different type now. Do not leave
        // its old importable name or a candidate-revision witness behind.
        let entry = GlobalName {
            module: module.canonical.name.clone(),
            name: "$operation".into(),
        };
        program.globals.insert(Global::Value(entry.clone()), body);
        next.revisions.remove(&module.canonical.name);
        let (next, extension) = self.extension_to(next)?;
        Ok((next, extension, entry))
    }
}
