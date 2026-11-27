//! Solver assignments are scoped to checked source nodes and lexical call frames.
//! Source spans locate metadata; they never identify a runtime activation.
use super::*;
use knot_checker::program::{NodeKind, NodeRef};
use std::collections::{HashMap, HashSet};
#[derive(Clone, Hash, PartialEq, Eq)]
pub(super) struct Identity {
    node: NodeRef,
    activation: usize,
    projection: String,
    ty: String,
}
type ProjectedPin = (TypedValue, Vec<projections::Projection>);

#[derive(Default)]
pub(super) struct Pins {
    /// Source nodes under a request that already fitted during this pass.
    /// They are left alone until the pass ends. Keyed without the type.
    pub(super) held: HashSet<(NodeRef, usize, String)>,
    assignments: HashMap<Identity, TypedValue>,
    pub(super) user: HashMap<Identity, TypedValue>,
    pub(super) repairing: Option<Identity>,
    partial: HashSet<Identity>,
}
impl Walker<'_> {
    pub(super) fn restart_attempt(
        &mut self,
        libraries: &knot_core::Program,
    ) -> Result<(), EditError> {
        self.probes
            .install_candidate(self.snapshot, self.snapshot, libraries)?;
        self.source_pins.restart_attempt();
        self.solver_pins = Pins::default();
        self.proposals.clear();
        self.scopes = vec![BTreeMap::new()];
        self.patches.clear();
        self.replacements.clear();
        self.recipes.clear();
        self.patch_locations.clear();
        self.changed_spans.clear();
        self.pending.clear();
        self.obligations.clear();
        self.guards.clear();
        self.source_values = Default::default();
        self.reset_user_pins();
        Ok(())
    }
    pub(super) fn source_identity(&self, site: &Site) -> Option<Identity> {
        let (root, construction) = if site.synthetic {
            let recipe = site.recipe.as_ref()?;
            (recipe.root.as_ref(), recipe.projection_identity())
        } else {
            (site, String::new())
        };
        let program = self
            .snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .chain(self.bundled.iter())
            .find(|p| p.canonical.name == root.module)?;
        let mut matches = program.nodes.iter().filter(|(_, info)| {
            matches!(info.kind, NodeKind::Expression) && info.span == root.expr.span
        });
        let (node, _) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let projection = projections::render("source".into(), &site.projections, None).ok()?;
        Some(Identity {
            node: program.node_ref(*node),
            activation: root.scope,
            projection: format!("{construction}:{projection}"),
            ty: site.ty.as_ref().map(Ty::source).unwrap_or_default(),
        })
    }
    fn assignment(&self, site: &Site, depth: usize) -> (Option<ProjectedPin>, bool) {
        if (self.solver_pins.assignments.is_empty()
            && self.solver_pins.user.is_empty()
            && self.solver_pins.partial.is_empty())
            || depth >= 128
        {
            return (None, false);
        }
        let mut partial = false;
        if let Some(id) = self.source_identity(site) {
            if self.solver_pins.repairing.as_ref() == Some(&id) {
                return (None, false);
            }
            // User locks are monomorphic entry bindings. An untyped syntax
            // projection may locate that same binding before recovering its type.
            let user = self
                .solver_pins
                .user
                .iter()
                .find(|(key, _)| {
                    key.node == id.node
                        && key.activation == id.activation
                        && key.projection == id.projection
                })
                .map(|(_, value)| value);
            if let Some(value) = user.or_else(|| self.solver_pins.assignments.get(&id)) {
                return (Some((value.clone(), vec![])), false);
            }
            partial = self.solver_pins.partial.contains(&id);
        }
        if !site.projections.is_empty() {
            let mut base = site.clone();
            let path = std::mem::take(&mut base.projections);
            base.ty = path[0].input.clone();
            let (value, partial) = self.assignment(&base, depth + 1);
            if let Some((value, mut prefix)) = value {
                prefix.extend(path);
                return (Some((value, prefix)), false);
            }
            if partial {
                return (None, true);
            }
        }
        if let CExpr::FieldAccess(base, field) = &site.expr.node {
            let projected = site
                .child(base)
                .projected(ProjectionKind::Field(field.clone()), site.ty.clone());
            return self.assignment(&projected, depth + 1);
        }
        let mut aliased = (None, false);
        if site.projections.is_empty() {
            if let CExpr::Var(reference) = &site.expr.node {
                if let Ref::Local(name) = reference {
                    if let Some(value) = self.scopes[site.scope].get(name) {
                        aliased = self.assignment(value, depth + 1);
                    }
                } else {
                    let name = syntax::reference(reference, &site.module);
                    if name.module == self.snapshot.graph.entry().program.canonical.name {
                        if let Some(f) = self.definition(&name) {
                            if f.params.is_empty() {
                                aliased = self.assignment(
                                    &Site {
                                        expr: f.body,
                                        module: name.module,
                                        scope: 0,
                                        ..site.clone()
                                    },
                                    depth + 1,
                                );
                            }
                        }
                    }
                }
            }
        }
        if aliased.0.is_some() {
            aliased
        } else {
            (None, partial || aliased.1)
        }
    }
    pub(super) fn solver_pin_value(
        &mut self,
        site: &Site,
    ) -> Result<Option<TypedValue>, EditError> {
        let Some((value, path)) = self.assignment(site, 0).0 else {
            return Ok(None);
        };
        if path.is_empty() {
            return Ok(Some(value));
        }
        let reference = self.probes.reference(&value)?;
        let source = projections::render(reference, &path, None)?;
        self.probes.declarations.clear();
        let (value, _) = self.probes.evaluate_lazy(&source, site.ty.as_ref())?;
        Ok(Some(value))
    }
    pub(super) fn solver_pinned(&self, site: &Site) -> bool {
        self.assignment(site, 0).0.is_some()
    }
    /// Whether this source node was frozen, whatever type the site carries.
    /// A site reached by walking syntax, not by unravelling, has no type yet.
    pub(super) fn frozen_node(&self, site: &Site) -> bool {
        let Some(id) = self.source_identity(site) else {
            return false;
        };
        self.solver_pins
            .held
            .contains(&(id.node.clone(), id.activation, id.projection.clone()))
            || self.solver_pins.assignments.keys().any(|key| {
                key.node == id.node
                    && key.activation == id.activation
                    && key.projection == id.projection
            })
    }
    /// Hold a source node for the rest of this pass.
    pub(super) fn hold_node(&mut self, site: &Site) {
        if let Some(id) = self.source_identity(site) {
            self.solver_pins
                .held
                .insert((id.node, id.activation, id.projection));
        }
    }
    pub(super) fn release_held(&mut self) {
        self.solver_pins.held.clear();
    }
    pub(super) fn is_held(&self, site: &Site) -> bool {
        self.source_identity(site).is_some_and(|id| {
            self.solver_pins
                .held
                .contains(&(id.node, id.activation, id.projection))
        })
    }
    pub(super) fn pin_source(&mut self, site: &Site, current: &str) -> Result<String, EditError> {
        if let Some(value) = self.solver_pin_value(site)? {
            self.probes.reference(&value)
        } else {
            Ok(current.into())
        }
    }
    pub(super) fn reject_partial_solver_pin(&self, site: &Site) -> Result<(), EditError> {
        if self.assignment(site, 0).1 {
            return unsupported("this rule requires a whole-value input state but the source has partial solver pins");
        }
        Ok(())
    }
    pub(super) fn freeze_solutions(&mut self) -> Result<(), EditError> {
        for mut site in std::mem::take(&mut self.proposals) {
            let value = self.source_value(&site)?;
            let ty = Ty::from_value(value.value_type());
            site.ty = Some(ty.clone());
            let identity = self.source_identity(&site).ok_or_else(|| {
                EditError::Unsupported(
                    "solver assignment needs an unambiguous checked source node and activation"
                        .into(),
                )
            })?;
            if self.solver_pins.assignments.contains_key(&identity)
                || self.solver_pins.user.contains_key(&identity)
            {
                continue;
            }
            if self.probes.has_structural_fields(&value)? && !self.materialized_assignment(&site) {
                // Structural routes keep independently chosen children fixed.
                // Do not turn a partial aggregate solution into a whole pin.
                self.solver_pins.partial.insert(identity);
                continue;
            }
            let reference = self.probes.reference(&value)?;
            let request_type = Ty::Named("Maybe".into(), vec![ty]);
            self.probes.declarations.clear();
            let (request, _) = self
                .probes
                .evaluate_lazy(&format!("Just {reference}"), Some(&request_type))?;
            // Exact pins use the checked Eq semantics, including rejection of
            // non-reflexive values. They never replace capture provenance.
            if !self.fits(&site, &request, &request_type)? {
                return Err(EditError::ValidationFailed);
            }
            self.obligations.push((site, request, request_type));
            self.solver_pins.assignments.insert(identity, value);
        }
        Ok(())
    }
}
