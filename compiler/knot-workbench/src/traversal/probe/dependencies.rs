//! Static executable dependencies of a concretely checked closure. Dictionary
//! globals come from elaboration, including transitive/default/structural code.
use super::*;
use knot_core::{Global, Kind};
use std::collections::BTreeSet;

impl Probes {
    pub(in crate::traversal) fn dependency_globals(
        &mut self,
        expression: &str,
        ty: &Ty,
        helpers: &str,
        visited: &mut usize,
    ) -> Result<BTreeSet<Global>, EditError> {
        self.declarations = helpers.into();
        let checked = self.check(&format!(
            "dependency :: {}\ndependency =\n{}",
            ty.source(),
            indent(expression)
        ))?;
        let phase = Instant::now();
        let linked = self
            .context
            .base
            .extend(&[checked.as_ref()])
            .map_err(EditError::Check)?;
        self.link_ms += phase.elapsed().as_secs_f64() * 1000.0;
        let core = linked.program();
        let root = Global::Value(GlobalName {
            module: checked.canonical.name.clone(),
            name: "dependency".into(),
        });
        let mut pending = vec![*core
            .globals
            .get(&root)
            .ok_or_else(|| EditError::Internal("missing dependency root".into()))?];
        let mut seen = BTreeSet::new();
        let mut globals = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if self
                .limits
                .runtime
                .cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(twine::Outcome::Cancelled.into());
            }
            *visited += 1;
            if *visited > self.limits.traversal.dependency_nodes {
                return unsupported("callback executable dependencies exceeded their node budget");
            }
            match &core.expressions[id.0].kind {
                Kind::Global(global) => {
                    globals.insert(global.clone());
                    if let Some(target) = core.globals.get(global) {
                        pending.push(*target);
                    }
                }
                Kind::Lam(_, body) | Kind::Select(body, _) => pending.push(*body),
                Kind::App(fun, args) => {
                    pending.push(*fun);
                    pending.extend(args);
                }
                Kind::Let(bindings, body) => {
                    pending.push(*body);
                    pending.extend(bindings.iter().map(|(_, value)| value));
                }
                Kind::Case(value, arms) => {
                    pending.push(*value);
                    pending.extend(arms.iter().map(|(_, body)| body));
                }
                Kind::Tuple(parts) => pending.extend(parts),
                Kind::Record(fields) => pending.extend(fields.iter().map(|(_, value)| value)),
                Kind::RecordUpdate(base, fields) => {
                    pending.push(*base);
                    pending.extend(fields.iter().map(|(_, value)| value));
                }
                Kind::Dict(fields) => pending.extend(fields.iter().map(|(_, value)| value)),
                Kind::Local(_)
                | Kind::Lit(_)
                | Kind::Rational(_)
                | Kind::Prim(_)
                | Kind::Host(_, _)
                | Kind::Ctor(_, _)
                | Kind::Unsupported(_) => {}
            }
        }
        Ok(globals)
    }
}
