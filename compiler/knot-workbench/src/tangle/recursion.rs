//! A use is recursive only when it connects two members of the same binding cycle.
use super::fallback::visit;
use knot_checker::{
    ast::{NodeId, TypedExpr},
    constrain::decl::scc_order,
    program::{BindingId, EvidenceSite, ResolvedReference, TypedProgram},
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct Recursion {
    pub bindings: HashSet<BindingId>,
    pub uses: HashSet<NodeId>,
}

impl Recursion {
    pub fn group(&mut self, program: &TypedProgram, members: &[(&[BindingId], &TypedExpr)]) {
        let mut owners = HashMap::new();
        for (index, (bindings, _)) in members.iter().enumerate() {
            owners.extend(bindings.iter().map(|id| (*id, index)));
        }
        let mut uses = vec![Vec::new(); members.len()];
        let mut edges = vec![Vec::new(); members.len()];
        for (from, (_, body)) in members.iter().enumerate() {
            visit(body, &mut |e| {
                if let Some(ResolvedReference::Binding(id)) =
                    program.references.get(&EvidenceSite::Expr(e.id))
                {
                    if let Some(&to) = owners.get(id) {
                        edges[from].push(to);
                        uses[from].push((e.id, to));
                    }
                }
            });
        }
        for component in scc_order(members.len(), &edges) {
            if component.len() == 1 && !edges[component[0]].contains(&component[0]) {
                continue;
            }
            for &from in &component {
                self.bindings.extend(members[from].0);
                self.uses.extend(
                    uses[from].iter().filter_map(|(site, to)| {
                        component.binary_search(to).is_ok().then_some(*site)
                    }),
                );
            }
        }
    }
}
