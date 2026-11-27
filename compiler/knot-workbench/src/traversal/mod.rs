//! Bounded source-directed pullbacks. Read-only definitions may be inspected;
//! edits belong to original entry-file expressions, never generated helpers.
mod adapters;
mod body;
use adapters::RegisteredAdapter;
pub use adapters::RequestAdapter;
mod call_frame;
pub use body::CallbackBody;
mod callbacks;
mod candidates;
mod higher_order;
mod lash;
mod list_route;
mod locks;
mod materialization;
mod recipes;
mod replacements;
mod solver_pins;
mod source_pins;
mod unravel_cache;
pub use higher_order::CallbackFactory;
mod constructor_rules;
pub use constructor_rules::ConstructorRule;
mod field_requests;
mod probe;
mod products;
pub use products::ProductRule;
use products::RegisteredProduct;
mod projection_updates;
mod projections;
mod values;
mod worklist;
use projections::ProjectionKind;
mod syntax;
mod template;
use crate::operation::TypedValue;
use crate::{
    edit_domain::{CheckedSourceChange, EditDomain},
    project::ProjectSnapshot,
    unravel::{EditError, EditLimits, LiteralPatch},
};
use knot_canonical::ast::{CDecl, CExpr, CFnDef, CPattern, CType, Ref};
use knot_core::{Field, GlobalName};
use knot_syntax::span::Spanned;
pub use probe::Data;
use probe::{global_source, indent, quoted, ProbeContext, Probes, Ty};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use syntax::{reference, Scopes, Site};

fn unsupported<T>(message: &str) -> Result<T, EditError> {
    Err(EditError::Unsupported(message.into()))
}

/// A request expression is checked in an internal module which imports the
/// project's modules by their full names. Callers must quote untrusted data.
pub struct Request {
    pub expression: String,
    pub type_name: String,
}
/// One entry in an ordered transaction. Request/value types may differ between
/// entries; their checked validators and lazy values remain separate.
#[derive(Clone, Copy)]
pub struct OutputRequest<'a> {
    pub output: &'a str,
    pub request: &'a Request,
}
pub struct SourceEditor {
    locked_bindings: Vec<String>,
    callbacks: Vec<CallbackFactory>,
    bodies: Vec<CallbackBody>,
    adapters: Vec<RequestAdapter>,
    constructor_rules: Vec<ConstructorRule>,
    products: Vec<ProductRule>,
    libraries: knot_core::Program,
    bundled: Vec<knot_checker::program::TypedProgram>,
}
/// Reusable checked-query environment, scoped to one immutable snapshot.
/// Retains bounded checked code and optionally an immutable runtime baseline.
/// Requests and candidate validation belong to each transaction.
#[derive(Clone)]
pub struct PreparedContext {
    list_hints: Option<Arc<std::sync::Mutex<Vec<knot_syntax::Span>>>>,
    reuse_route: bool,
    snapshot: String,
    edit_domain: EditDomain,
    probes: Arc<ProbeContext>,
    baseline: BaselineStats,
}
/// Preparation is paid once per context, separately from each edit's budget.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct BaselineStats {
    pub work: u64,
    pub shared_heap_pages: usize,
    pub reused_globals: usize,
}
impl PreparedContext {
    /// Keep one bounded linked baseline for successive picks in a preview.
    pub fn with_route_reuse(mut self) -> Self {
        self.reuse_route = true;
        self
    }

    /// Learn list element positions from accepted edits against this baseline.
    pub fn with_list_routes(mut self) -> Self {
        self.list_hints = Some(Arc::default());
        self
    }

    pub fn edit_domain(&self) -> &EditDomain {
        &self.edit_domain
    }
    /// Install prospective permissions before running a transaction. Runtime
    /// evidence remains fully enabled until an executable summary is supported.
    pub fn with_edit_domain(mut self, domain: EditDomain) -> Result<Self, EditError> {
        if domain.snapshot() != self.snapshot {
            return Err(EditError::StaleRevision);
        }
        self.edit_domain = domain;
        Ok(self)
    }
    pub fn baseline_stats(&self) -> BaselineStats {
        self.baseline
    }

    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }
}
impl SourceEditor {
    pub fn new() -> Result<Self, EditError> {
        Ok(Self {
            locked_bindings: vec![],
            callbacks: vec![],
            bodies: vec![],
            adapters: vec![],
            constructor_rules: vec![],
            products: vec![],
            bundled: knot_canonical::prelude::library_modules()
                .iter()
                .map(|(name, _)| {
                    knot_checker::check::check_library_program(name)
                        .map_err(|e| EditError::Check(format!("{e:?}")))
                })
                .collect::<Result<_, _>>()?,
            libraries: crate::libraries::Libraries::bundled(None)
                .map_err(EditError::Check)?
                .core,
        })
    }
    /// Keep these entry bindings at their original values. Computed locks may
    /// be repaired through free dependencies; they do not freeze every input.
    pub fn with_locked_bindings(mut self, bindings: Vec<String>) -> Self {
        self.locked_bindings = bindings;
        self
    }
    /// Explicit single-step translations. Original and translated requests both
    /// remain final obligations. Matching failure never tries another adapter.
    pub fn with_request_adapters(mut self, adapters: Vec<RequestAdapter>) -> Self {
        self.adapters = adapters;
        self
    }
    /// Ordinary unravel contracts, in attachment order for each constructor.
    pub fn with_constructor_rules(mut self, rules: Vec<ConstructorRule>) -> Self {
        self.constructor_rules = rules;
        self
    }
    /// Ordinary field-feed rules for a concrete tuple or record type.
    pub fn with_product_rules(mut self, rules: Vec<ProductRule>) -> Self {
        self.products = rules;
        self
    }
    /// Explicit unravel factories for fixed callback positions of named calls.
    /// Factories receive the actual source callback. They return an unravel
    /// function; the engine supplies the matching forward field itself.
    pub fn with_callback_factories(mut self, callbacks: Vec<CallbackFactory>) -> Self {
        self.callbacks = callbacks;
        self
    }
    /// Compile callback bodies under an explicit type witness. Generated
    /// companions retain guarded candidates and require Eq on the input.
    pub fn with_callback_bodies(mut self, bodies: Vec<CallbackBody>) -> Self {
        self.bodies = bodies;
        self
    }
    pub fn prepare(
        &self,
        snapshot: &ProjectSnapshot,
        output: &str,
        request: &Request,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        self.prepare_many(
            snapshot,
            &[OutputRequest { output, request }],
            roots,
            limits,
        )
    }
    pub fn prepare_many(
        &self,
        snapshot: &ProjectSnapshot,
        requests: &[OutputRequest<'_>],
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        if snapshot.sources()[snapshot.entry_path()].len() > limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        if limits
            .runtime
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        let started = Instant::now();
        let context = self.context(snapshot)?;
        let context_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut edit = self.prepare_many_in(snapshot, &context, requests, roots, limits)?;
        edit.timings.setup_ms += context_ms;
        edit.timings.total_ms += context_ms;
        Ok(edit)
    }
    pub fn context(&self, snapshot: &ProjectSnapshot) -> Result<PreparedContext, EditError> {
        Ok(PreparedContext {
            list_hints: None,
            reuse_route: false,
            snapshot: snapshot.id().into(),
            edit_domain: EditDomain::entry(snapshot, usize::MAX)?,
            probes: Arc::new(ProbeContext::new(snapshot, &self.libraries)?),
            baseline: BaselineStats::default(),
        })
    }
    /// Prepare an optional immutable runtime baseline against this exact checked
    /// snapshot. Compatibility misses leave a fresh context. Preparation has its
    /// own explicit budget; exhaustion/cancellation return an error, never a
    /// silently renewed allowance. Each later edit pays for its own fork.
    #[cfg(feature = "incremental")]
    pub fn context_reusing(
        &self,
        snapshot: &ProjectSnapshot,
        previous: &crate::operation::OperationSession,
        limits: twine::Limits,
    ) -> Result<PreparedContext, EditError> {
        if limits
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        let mut probes = ProbeContext::new(snapshot, &self.libraries)?;
        let baseline = probes.seed(previous, limits)?;
        Ok(PreparedContext {
            list_hints: None,
            reuse_route: false,
            snapshot: snapshot.id().into(),
            edit_domain: EditDomain::entry(snapshot, usize::MAX)?,
            probes: Arc::new(probes),
            baseline,
        })
    }
    pub fn prepare_in(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        output: &str,
        request: &Request,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        self.prepare_many_in(
            snapshot,
            context,
            &[OutputRequest { output, request }],
            roots,
            limits,
        )
    }
    /// Apply a checked request constructor to a separately checked input value.
    /// The constructor's type_name describes its result, just as for prepare_in.
    pub fn prepare_call_in(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        output: &str,
        constructor: &Request,
        argument: &Request,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        self.prepare_requests(
            snapshot,
            context,
            &[OutputRequest {
                output,
                request: constructor,
            }],
            Some(argument),
            roots,
            limits,
        )
    }
    // Preloading reserves an initial block of internal module and export names.
    // Use ordinary linking for clients whose code mentions that namespace.
    fn can_preload_queries(
        &self,
        snapshot: &ProjectSnapshot,
        output: OutputRequest<'_>,
        input: Option<&Request>,
    ) -> bool {
        let reserved =
            |s: &str| s.contains("TwineTraversalLive") || s.contains("twineLiveOperation");
        !std::iter::once(output.request)
            .chain(input)
            .any(|r| reserved(&r.expression) || reserved(&r.type_name))
            && !reserved(output.output)
            && !snapshot.sources().values().any(|s| reserved(s))
            && !self.callbacks.iter().any(|c| reserved(&c.expression))
            && !self.bodies.iter().any(|b| reserved(&b.contract))
            && !self.adapters.iter().any(|a| reserved(&a.expression))
            && !self
                .constructor_rules
                .iter()
                .any(|r| reserved(&r.expression))
            && !self
                .products
                .iter()
                .any(|p| reserved(&p.expression) || reserved(&p.value_type))
    }
    pub fn prepare_many_in(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        self.prepare_requests(snapshot, context, requests, None, roots, limits)
    }
    fn prepare_requests(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
    ) -> Result<SourceEdit, EditError> {
        self.prepare_lashing(snapshot, context, requests, argument, roots, limits, 0, None)
    }
    fn prepare_requests_once(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
        snag: &lash::SnagSlot,
    ) -> Result<SourceEdit, EditError> {
        if requests.is_empty() {
            return unsupported("a source transaction needs at least one output request");
        }
        if requests.len() > limits.traversal.source_sites {
            return unsupported("output requests exceed the source site budget");
        }
        if context.snapshot != snapshot.id() {
            return Err(EditError::StaleRevision);
        }
        let started = Instant::now();
        let original = &snapshot.sources()[snapshot.entry_path()];
        if original.len() > limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        let list_hints = context
            .list_hints
            .as_ref()
            .filter(|_| requests.len() == 1 && self.locked_bindings.is_empty());
        let mut w = Walker {
            list_hints: list_hints
                .map(|h| h.lock().unwrap().clone())
                .unwrap_or_default(),
            list_route_hits: 0,
            list_route_misses: 0,
            snapshot,
            edit_domain: &context.edit_domain,
            callback_factories: &self.callbacks,
            callback_bodies: &self.bodies,
            constructor_rules: &self.constructor_rules,
            source_pins: Default::default(),
            solver_pins: Default::default(),
            user_locks: vec![],
            lock_repairs: 0,
            proposals: vec![],
            collect_proposals: false,
            adapters: vec![],
            products: vec![],
            bundled: &self.bundled,
            probes: Probes::from_context(context.probes.clone(), limits.clone()),
            scopes: vec![BTreeMap::new()],
            patches: BTreeMap::new(),
            replacements: BTreeMap::new(),
            recipes: BTreeMap::new(),
            patch_locations: BTreeMap::new(),
            changed_spans: Default::default(),
            visits: 0,
            body_nodes: 0,
            body_depth: 0,
            pending: vec![],
            source_values: Default::default(),
            unravel_cache: Default::default(),
            obligations: vec![],
            spliced: Default::default(),
            patch_via: BTreeMap::new(),
            snag: snag.clone(),
            guards: vec![],
        };
        if context.reuse_route
            && requests.len() == 1
            && self.can_preload_queries(snapshot, requests[0], argument)
        {
            w.probes.enable_route()?;
        }
        higher_order::validate_factories(&self.callbacks, &self.bodies)?;
        w.reject_retired_materializers()?;
        w.register_adapters(&self.adapters)?;
        w.register_constructor_rules()?;
        w.register_products(&self.products)?;
        // Check all roots against the immutable revision. Function-bearing
        // request values keep their original captures through final validation.
        let outputs = requests
            .iter()
            .map(|output| w.prepare_output(*output, argument))
            .collect::<Result<Vec<_>, _>>()?;
        w.prepare_user_locks(&self.locked_bindings)?;
        let setup_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut route_ms = 0.0;
        let mut candidate_check_ms = 0.0;
        let mut validation_ms = 0.0;
        let attempts = if outputs.len() > 1 {
            limits.traversal.solve_attempts.min(2)
        } else {
            limits.traversal.solve_attempts.min(1)
        };
        if attempts == 0 {
            return unsupported("source solving needs at least one attempt");
        }
        let mut accepted = None;
        for attempt in 0..attempts {
            if attempt > 0 {
                w.restart_attempt(&self.libraries)?;
            }
            let mut order = (0..outputs.len()).collect::<Vec<_>>();
            if attempt > 0 {
                order.reverse();
            }
            let result = (|| {
                let mut latest = None;
                for (position, index) in order.into_iter().enumerate() {
                    let phase = Instant::now();
                    let (site, request, ty) = &outputs[index];
                    w.collect_proposals = position + 1 < outputs.len() || !w.user_locks.is_empty();
                    let routed = w.walk(site.clone(), request.clone(), ty.clone());
                    route_ms += phase.elapsed().as_secs_f64() * 1000.0;
                    routed?;
                    let (candidate, patches, check, validation) =
                        w.validate_candidate(roots, &self.libraries)?;
                    candidate_check_ms += check;
                    validation_ms += validation;
                    if w.collect_proposals {
                        let phase = Instant::now();
                        w.freeze_solutions()?;
                        validation_ms += phase.elapsed().as_secs_f64() * 1000.0;
                    }
                    latest = Some((candidate, patches));
                    if let Some(repair) = w.repair_user_locks(roots, &self.libraries)? {
                        candidate_check_ms += repair.check_ms;
                        validation_ms += repair.validation_ms;
                        latest = Some((repair.candidate, repair.patches));
                    }
                }
                if !w.user_locks.is_empty() {
                    let phase = Instant::now();
                    w.validate_obligations(false)?;
                    w.validate_user_locks()?;
                    validation_ms += phase.elapsed().as_secs_f64() * 1000.0;
                }
                Ok(latest.unwrap())
            })();
            match result {
                Ok(candidate) => {
                    accepted = Some(candidate);
                    break;
                }
                Err(
                    EditError::RuleFailed
                    | EditError::ValidationFailed
                    | EditError::ConflictingRequests,
                ) if attempt + 1 < attempts => {}
                Err(error) => return Err(error),
            }
        }
        let (candidate, patches) = accepted.ok_or(EditError::ValidationFailed)?;
        let source_change = context
            .edit_domain
            .certify(snapshot, &candidate, &patches)?;
        w.probes.publish_route();
        if let Some(hints) = list_hints {
            *hints.lock().unwrap() = patches.iter().take(32).map(|p| p.span).collect();
        }
        Ok(SourceEdit {
            snapshot: snapshot.id().into(),
            source_change,
            candidate,
            candidate_program: w.probes.candidate_program(),
            patches,
            usage: w.probes.usage,
            timings: SourceTimings {
                total_ms: started.elapsed().as_secs_f64() * 1000.0,
                setup_ms,
                route_ms,
                candidate_check_ms,
                validation_ms,
                query_check_ms: w.probes.check_ms,
                query_link_ms: w.probes.link_ms,
                query_run_ms: w.probes.run_ms,
                runtime_forks: w.probes.runtime_forks,
                runtime_fork_work: w.probes.runtime_fork_work,
                queries: w.probes.count,
                query_cache_hits: w.probes.cache_hits,
                linked_route_hits: w.probes.route_hits,
                result_cache_hits: 0,
                unravel_cache_hits: w.unravel_cache.hits,
                validation_cache_hits: w.probes.validation_cache_hits(),
                list_route_hits: w.list_route_hits,
                list_route_misses: w.list_route_misses,
                sites: w.visits,
            },
        })
    }
}

fn preserve_types(
    old: &knot_checker::program::TypedProgram,
    new: &knot_checker::program::TypedProgram,
) -> Result<(), EditError> {
    let mut a = old.substitution.clone();
    let mut b = new.substitution.clone();
    for binding in old.bindings.values().filter(|b| b.global.is_some()) {
        let name = &binding.name;
        let key = knot_checker::solve::SchemeKey::TopLevel(name.clone());
        if let Some(next) = new.schemes.get(&key) {
            if knot_checker::print::print_scheme(&mut a, &binding.scheme)
                != knot_checker::print::print_scheme(&mut b, next)
            {
                return Err(EditError::Check(format!(
                    "edit changes inferred type of {name}"
                )));
            }
        }
    }
    Ok(())
}
/// Wall-clock phase timings. Query subtotals overlap the encompassing phases.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct SourceTimings {
    pub total_ms: f64,
    pub setup_ms: f64,
    pub route_ms: f64,
    pub candidate_check_ms: f64,
    pub validation_ms: f64,
    // Query subtotals overlap the phases above; they are not additional phases.
    pub query_check_ms: f64,
    pub query_link_ms: f64,
    pub query_run_ms: f64,
    /// Query sessions forked from an immutable checked runtime baseline.
    pub runtime_forks: usize,
    /// Includes successful and missed attempts; already charged in EditUsage.
    pub runtime_fork_work: u64,
    pub queries: usize,
    pub query_cache_hits: usize,
    pub linked_route_hits: usize,
    /// Compatibility counter: decoded-result replay is retired; always zero.
    pub result_cache_hits: usize,
    /// Completed shared-global reverse expansions reused in the same wave.
    pub unravel_cache_hits: usize,
    /// Successful validation batches reused after exact dependency checks.
    pub validation_cache_hits: usize,
    pub list_route_hits: usize,
    pub list_route_misses: usize,
    pub sites: usize,
}
/// Private construction guarantees validation precedes publication. The caller
/// owns file saving and must compare the complete snapshot immediately before it.
pub struct SourceEdit {
    snapshot: String,
    source_change: CheckedSourceChange,
    candidate: Arc<ProjectSnapshot>,
    candidate_program: Arc<knot_core::Program>,
    patches: Vec<LiteralPatch>,
    usage: crate::unravel::EditUsage,
    timings: SourceTimings,
}
impl SourceEdit {
    /// Exact source confinement for this accepted transition. This token does
    /// not establish executable equivalence or permit importing runtime values.
    pub fn source_change(&self) -> &CheckedSourceChange {
        &self.source_change
    }
    pub fn timings(&self) -> SourceTimings {
        self.timings
    }
    pub fn snapshot(&self) -> &str {
        &self.snapshot
    }
    pub fn candidate_source(&self) -> &str {
        &self.candidate.sources()[self.candidate.entry_path()]
    }
    /// The exact checked project that passed final validation, including its
    /// generated exports. Consumers can render it without checking source again.
    /// This does not publish the edit or prove that disk sources are still current.
    pub fn candidate_snapshot(&self) -> &Arc<ProjectSnapshot> {
        &self.candidate
    }
    /// Linked code for `candidate_snapshot`, without transaction query helpers
    /// or speculative runtime state. A receiving runtime must independently
    /// validate any reuse; this code does not authorize importing old handles.
    pub fn candidate_program(&self) -> &Arc<knot_core::Program> {
        &self.candidate_program
    }
    /// Typed exports of the exact validated candidate. No unchecked Core or
    /// caller-provided types can replace the candidate's code/type association.
    pub fn candidate_operations(
        &self,
        names: &[&str],
    ) -> Result<crate::operation::PreparedOperations, String> {
        crate::operation::PreparedOperations::from_linked(
            self.candidate_program.clone(),
            &self.candidate.graph.entry().program,
            names,
        )
    }
    pub fn patches(&self) -> &[LiteralPatch] {
        &self.patches
    }
    pub fn usage(&self) -> crate::unravel::EditUsage {
        self.usage
    }
}

struct Walker<'a> {
    list_hints: Vec<knot_syntax::Span>,
    list_route_hits: usize,
    list_route_misses: usize,
    edit_domain: &'a EditDomain,
    callback_factories: &'a [CallbackFactory],
    callback_bodies: &'a [CallbackBody],
    source_pins: source_pins::SourcePins,
    solver_pins: solver_pins::Pins,
    user_locks: Vec<locks::Lock>,
    lock_repairs: usize,
    proposals: Vec<Site>,
    collect_proposals: bool,
    adapters: Vec<RegisteredAdapter>,
    constructor_rules: &'a [ConstructorRule],
    products: Vec<RegisteredProduct>,
    snapshot: &'a ProjectSnapshot,
    bundled: &'a [knot_checker::program::TypedProgram],
    probes: Probes,
    scopes: Scopes,
    patches: BTreeMap<(u32, u32), LiteralPatch>,
    replacements: BTreeMap<(u32, u32), Spanned<CExpr>>,
    recipes: BTreeMap<(u32, u32), recipes::PendingRecipe>,
    patch_locations: recipes::Locations,
    changed_spans: std::collections::BTreeSet<(u32, u32)>,
    visits: usize,
    body_nodes: usize,
    body_depth: usize,
    pending: Vec<worklist::Work>,
    source_values: values::SourceValues,
    unravel_cache: unravel_cache::UnravelCache,
    // Retain each input obligation, including unchanged siblings and pins.
    obligations: Vec<(Site, TypedValue, Ty)>,
    guards: Vec<(Site, String)>,
    /// Spans replaced by a bubbled splice. Patches inside them are dropped.
    spliced: std::collections::BTreeSet<(u32, u32)>,
    /// The call each literal patch was reached through, if any.
    patch_via: BTreeMap<(u32, u32), Arc<syntax::Caller>>,
    /// Where a lasher could be added after the first snag of this attempt.
    snag: lash::SnagSlot,
}
/// A rule's answer, read from its `Unravelled` value.
pub(in crate::traversal) enum RuleAnswer {
    Unravel(TypedValue),
    Splice,
    Snag,
}
impl Walker<'_> {
    fn validate_candidate(
        &mut self,
        roots: &[std::path::PathBuf],
        libraries: &knot_core::Program,
    ) -> Result<(Arc<ProjectSnapshot>, Vec<LiteralPatch>, f64, f64), EditError> {
        let recipes::Construction { patches, locations } = self.finish_recipes()?;
        self.validate_source_pins()?;
        let phase = Instant::now();
        let candidate = self.edit_domain.apply(self.snapshot, &patches)?;
        if candidate.len() > self.probes.limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        let updated = ProjectSnapshot::load_reusing(
            self.snapshot.entry_path(),
            roots,
            &Default::default(),
            Some(candidate),
            self.snapshot.entry_extension(),
            Some(self.snapshot),
        )
        .map_err(EditError::Check)?;
        for (path, source) in updated.sources() {
            if path != self.snapshot.entry_path()
                && self.snapshot.sources().get(path) != Some(source)
            {
                return Err(EditError::StaleRevision);
            }
        }
        preserve_types(
            &self.snapshot.graph.entry().program,
            &updated.graph.entry().program,
        )?;
        self.edit_domain
            .certify(self.snapshot, &updated, &patches)?;
        self.replacements = replacements::checked(
            &patches,
            &locations,
            &updated.graph.entry().program.canonical,
        )?;
        let candidate_check_ms = phase.elapsed().as_secs_f64() * 1000.0;
        let phase = Instant::now();
        self.probes
            .install_candidate(self.snapshot, &updated, libraries)?;
        self.begin_candidate_values();
        self.validate_obligations(true)?;
        Ok((
            Arc::new(updated),
            patches,
            candidate_check_ms,
            phase.elapsed().as_secs_f64() * 1000.0,
        ))
    }
    pub(super) fn validate_obligations(&mut self, skip_user_locks: bool) -> Result<(), EditError> {
        // Check root and every intermediate input obligation on the actual
        // candidate. A shared edit can invalidate an earlier satisfied sibling.
        let mut batch = Vec::new();
        let mut bytes = 0;
        let obligations = std::mem::take(&mut self.obligations);
        let guards = std::mem::take(&mut self.guards);
        for chunk in obligations.chunks(16) {
            // Names describe positions within this bounded helper, not the
            // transaction's visit count. Equal helper shapes can then reuse code
            // while each invocation receives its own typed request handles.
            for (i, (site, r, t)) in chunk.iter().enumerate() {
                if skip_user_locks && self.is_user_lock_site(site) {
                    continue;
                }
                let value = self.value_declaration(
                    site,
                    &format!("current{i}"),
                    &format!("twineObligation{i}Type"),
                )?;
                let request_ref = self.probes.reference(r)?;
                let declarations = format!(
                    "{value}request{i} :: {}\nrequest{i} = {request_ref}\n",
                    t.source()
                );
                self.probes.add_validation(
                    &mut batch,
                    &mut bytes,
                    declarations,
                    format!("fits request{i} current{i}"),
                )?;
            }
            self.probes.flush_validation(&mut batch)?;
            bytes = 0;
        }
        for (i, (site, predicate)) in guards.iter().enumerate() {
            let declarations =
                self.value_declaration(site, &format!("guard{i}"), &format!("twineGuard{i}Type"))?;
            self.probes.add_validation(
                &mut batch,
                &mut bytes,
                declarations,
                format!("let current = guard{i} in {predicate}"),
            )?;
        }
        self.probes.flush_validation(&mut batch)?;
        self.obligations = obligations;
        self.guards = guards;
        Ok(())
    }
    fn prepare_output(
        &mut self,
        output: OutputRequest<'_>,
        argument: Option<&Request>,
    ) -> Result<(Site, TypedValue, Ty), EditError> {
        let (ty, call) = if let Some(input) = argument {
            self.probes.declarations = format!(
                "input :: {}\ninput =\n{}",
                input.type_name,
                indent(&input.expression)
            );
            let (value, _) = self.probes.evaluate_lazy("input", None)?;
            let reference = self.probes.reference(&value)?;
            (
                format!("({}) -> ({})", input.type_name, output.request.type_name),
                format!("requested {reference}"),
            )
        } else {
            (output.request.type_name.clone(), "requested".into())
        };
        self.probes.declarations = format!(
            "requested :: {ty}\nrequested =\n{}",
            indent(&output.request.expression)
        );
        let (r, t) = self.probes.evaluate_lazy(&call, None)?;
        self.probes.declarations.clear();
        let site = self.output_site(output.output)?;
        Ok((site, r, t))
    }
    pub(super) fn output_site(&mut self, output: &str) -> Result<Site, EditError> {
        let entry = &self.snapshot.graph.entry().program.canonical.name;
        let g = GlobalName {
            module: entry.clone(),
            name: output.into(),
        };
        let checked = self
            .probes
            .check(&format!("probe = {}\n", global_source(&g)))?;
        let ty = Ty::read(
            &mut checked.substitution.clone(),
            checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("probe".into()))
                .unwrap()
                .ty,
        )?;
        let f = self
            .definition(&g)
            .ok_or_else(|| EditError::Unsupported("output binding not found".into()))?;
        let site = Site {
            recipe: None,
            candidate_source: false,
            via: None,
            lash: None,
            synthetic: false,
            projections: vec![],
            module: entry.clone(),
            expr: f.body,
            scope: 0,
            locked: false,
            fixed: false,
            ty: Some(ty),
        };
        Ok(site)
    }
    fn source(&self, s: &Site) -> Result<String, EditError> {
        syntax::render_current(
            s,
            &self.scopes,
            &self.replacements,
            &self.snapshot.graph.entry().program.canonical.name,
        )
    }
    fn definition(&self, g: &GlobalName) -> Option<CFnDef> {
        self.snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .chain(self.bundled.iter())
            .find(|p| p.canonical.name == g.module)?
            .canonical
            .decls
            .iter()
            .find_map(|d| match &d.node {
                CDecl::Fn(f) if f.name == g.name => Some(f.clone()),
                _ => None,
            })
    }
    fn fits(&mut self, s: &Site, r: &TypedValue, t: &Ty) -> Result<bool, EditError> {
        debug_assert_eq!(*t, Ty::from_value(r.value_type()));
        let current = self.source_value(s)?;
        let value = self.probes.call_typed(
            "Prelude.fits",
            &[r.clone(), current],
            &Ty::Named("Bool".into(), vec![]),
        )?;
        match self.probes.decode_live(&value)? {
            Data::Constructor(n, _) if n.module == ["Prelude"] => Ok(n.name == "True"),
            _ => Err(EditError::Internal("non-Bool validator".into())),
        }
    }
    fn walk_one(&mut self, s: Site, r: TypedValue, t: Ty) -> Result<(), EditError> {
        let mut completions = Vec::new();
        let result = self.walk_expansion(s, r, t, &mut completions);
        if result.is_ok() && !completions.is_empty() {
            // Worklist reversal runs children first, then this continuation.
            self.pending.push(worklist::Work::Complete(completions));
        }
        result
    }
    fn walk_expansion(
        &mut self,
        mut s: Site,
        mut r: TypedValue,
        mut t: Ty,
        completions: &mut Vec<usize>,
    ) -> Result<(), EditError> {
        loop {
            self.visit_site()?;
            self.obligations.push((s.clone(), r.clone(), t.clone()));
            if self.fits(&s, &r, &t)? {
                return Ok(());
            }
            if s.fixed {
                return unsupported("captured callback value is pinned");
            }
            if self.solver_pinned(&s) {
                return Err(EditError::ValidationFailed);
            }
            if self.collect_proposals {
                self.proposals.push(s.clone());
            }
            if let Some((request, ty)) = self.adapt_request(&s, &r, &t)? {
                r = request;
                t = ty;
                continue;
            }
            if !s.projections.is_empty() {
                match self.focus_projection(s)? {
                    projections::Focus::Direct(site) => s = site,
                    projections::Focus::Computed(site) => {
                        let (base, request, ty) = self.lift_projection(site, &r, &t)?;
                        s = base;
                        r = request;
                        t = ty;
                    }
                }
                continue;
            }
            if matches!(
                s.expr.node,
                CExpr::Tuple(_) | CExpr::Record(_) | CExpr::RecordUpdate(_, _)
            ) {
                if let Some((children, rule, input)) = self.default_product_rule(&s, &t)? {
                    self.remember_product_fields(&s, &children)?;
                    return self.apply_field_rule(&s, &t, children, r, input, &rule);
                }
                (r, t) = self.product_requests(&s, r, t)?;
            }
            return match &s.expr.node {
                CExpr::Annotated(_, e) => {
                    s = Site {
                        ty: s.ty.clone(),
                        lash: Some(s.lash_root()),
                        ..s.child(e)
                    };
                    continue;
                }
                CExpr::Var(Ref::Local(n)) => {
                    let mut next = self.scopes[s.scope]
                        .get(n)
                        .cloned()
                        .ok_or_else(|| EditError::Unsupported(format!("missing local {n}")))?;
                    if next.ty.is_none() {
                        next.ty = s.ty.clone();
                    }
                    next.fixed |= s.fixed;
                    // The name stands for the same value as the expression
                    // it is bound to, so it continues the use's chain.
                    if s.lash.is_some() {
                        next.lash = s.lash.clone();
                    }
                    s = next;
                    continue;
                }
                CExpr::Var(reference_) => {
                    let g = reference(reference_, &s.module);
                    if let Some(f) = self.definition(&g) {
                        if f.params.is_empty()
                            && g.module == self.snapshot.graph.entry().program.canonical.name
                        {
                            if self.shared_global(&s, &g, &r, &t, completions)? {
                                return Ok(());
                            }
                            s = Site {
                                recipe: None,
                                candidate_source: false,
                                via: s.via.clone(),
                                lash: Some(s.lash_root()),
                                synthetic: false,
                                projections: vec![],
                                module: g.module,
                                expr: f.body,
                                scope: 0,
                                locked: s.locked,
                                fixed: s.fixed,
                                ty: s.ty.clone(),
                            };
                            continue;
                        }
                    }
                    self.materialize(&s, &r, &t)
                }
                CExpr::If(condition, yes, no) => {
                    let condition = s.child(condition);
                    let taken = self.condition(&condition)?;
                    let mut selected = s.child(if taken { yes } else { no });
                    selected.ty = s.ty.clone();
                    selected.lash = Some(s.lash_root());
                    s = selected;
                    continue;
                }
                CExpr::Negate(e) if !matches!(e.node, CExpr::IntLit(_) | CExpr::FloatLit(_)) => {
                    let fun = s.child(&Spanned::new(
                        s.expr.span,
                        CExpr::Var(Ref::Builtin("negate".into())),
                    ));
                    let expr = Spanned::new(s.expr.span, CExpr::App(Box::new(fun.expr), e.clone()));
                    self.call(Site { expr, ..s }, r, t)
                }
                CExpr::IntLit(_)
                | CExpr::FloatLit(_)
                | CExpr::StringLit(_)
                | CExpr::Unit
                | CExpr::Negate(_)
                | CExpr::Ctor(_) => self.materialize(&s, &r, &t),
                CExpr::Tuple(es) => {
                    let Ty::Tuple(ts) = t else {
                        return unsupported("tuple traversal needs fieldwise constraints");
                    };
                    if es.len() != ts.len() {
                        return unsupported("tuple request arity mismatch");
                    }
                    let types = match &s.ty {
                        Some(Ty::Tuple(ts)) => Some(ts),
                        _ => None,
                    };
                    for (i, (e, t)) in es.iter().zip(ts).enumerate() {
                        let r = self.probes.project_live(&r, Field::Index(i))?;
                        let mut c = s.child(e);
                        c.ty = types.map(|ts| ts[i].clone());
                        self.schedule(c, r, t)?;
                    }
                    Ok(())
                }
                CExpr::Record(fs) => {
                    let Ty::Record(ts) = t else {
                        return unsupported("record traversal needs fieldwise constraints");
                    };
                    if fs.len() != ts.len() || fs.iter().any(|(n, _)| !ts.contains_key(n)) {
                        return unsupported("record request fields differ");
                    }
                    for (n, e) in fs {
                        let r = self.probes.project_live(&r, Field::Named(n.clone()))?;
                        let mut c = s.child(e);
                        if let Some(Ty::Record(types)) = &s.ty {
                            c.ty = types.get(n).cloned();
                        }
                        self.schedule(c, r, ts[n].clone())?;
                    }
                    Ok(())
                }
                CExpr::List(_) => self.list(&s, r, t),
                CExpr::App(_, _) | CExpr::BinOp(_, _, _) => self.call(s, r, t),
                CExpr::Let(bs, b) => {
                    let root = s.lash_root();
                    s = self.let_site(&s, bs, b)?;
                    s.lash = Some(root);
                    continue;
                }
                CExpr::Case(value, arms) => {
                    let root = s.lash_root();
                    s = self.case_site(&s, value, arms)?;
                    s.lash = Some(root);
                    continue;
                }
                CExpr::FieldAccess(base, name) => {
                    let projected = s
                        .child(base)
                        .projected(ProjectionKind::Field(name.clone()), s.ty.clone());
                    s = projected;
                    continue;
                }
                CExpr::RecordUpdate(_, _) => {
                    let Ty::Record(types) = t else {
                        return unsupported("record update needs fieldwise constraints");
                    };
                    for (name, request_type) in &types {
                        let request = self.probes.project_live(&r, Field::Named(name.clone()))?;
                        let output = match &s.ty {
                            Some(Ty::Record(fields)) => fields.get(name).cloned(),
                            _ => None,
                        };
                        let focused = s.projected(ProjectionKind::Field(name.clone()), output);
                        self.schedule(focused, request, request_type.clone())?;
                    }
                    Ok(())
                }
                _ => unsupported("no source route for this expression and constraint"),
            };
        }
    }
    fn materialize(&mut self, s: &Site, r: &TypedValue, t: &Ty) -> Result<(), EditError> {
        self.reject_partial_solver_pin(s)?;
        if s.synthetic {
            return self.materialize_recipe(s, r, t);
        }
        let module = self
            .snapshot
            .graph
            .modules
            .iter()
            .find(|m| m.program.canonical.name == s.module)
            .ok_or_else(|| EditError::Unsupported("unknown source module".into()))?;
        if s.locked {
            return unsupported("this source expression is pinned");
        }
        let Some(before) = self.snapshot.editable_source(&module.path, s.expr.span) else {
            // A replacement that cannot be made in read-only source is made at
            // the call that led there, with that call's own request.
            let Some(caller) = s.via.clone() else {
                return unsupported("this expression is in a read-only module");
            };
            return self.splice_call(&caller);
        };
        let before = before.to_string();
        let candidate = self.choose_candidate(s, r, t)?;
        let mut replacement = candidate.edit;
        let source = &self.snapshot.sources()[self.snapshot.entry_path()];
        replacement = materialization::at_column(
            &replacement,
            source,
            s.expr.span.start as usize,
            self.probes.limits.source_bytes,
        )?;
        let key = (s.expr.span.start, s.expr.span.end);
        if self
            .spliced
            .iter()
            .any(|&(a, b)| a <= key.0 && key.1 <= b && (a, b) != key)
        {
            // The enclosing call is already being replaced as a whole.
            return Ok(());
        }
        if let Some(previous) = self.patches.get(&key) {
            if previous.replacement != replacement {
                return Err(EditError::ConflictingRequests);
            }
        } else {
            if self.patches.keys().any(|&(a, b)| a < key.1 && key.0 < b) {
                return unsupported("overlapping source patches");
            }
            self.changed_spans.insert(key);
            if let Some(via) = &s.via {
                self.patch_via.insert(key, via.clone());
            }
            self.patch_locations.insert(
                key,
                vec![(key, knot_syntax::Span::new(0, replacement.len() as u32))],
            );
            self.patches.insert(
                key,
                LiteralPatch {
                    span: s.expr.span,
                    before,
                    replacement,
                },
            );
        }
        Ok(())
    }
    /// Replace a whole call because something inside its read-only body had
    /// to be replaced. Edits already routed out through its arguments are
    /// dropped: the printed value takes their place.
    fn splice_call(&mut self, caller: &Arc<syntax::Caller>) -> Result<(), EditError> {
        // Requests recorded on the way through this call no longer apply:
        // the call is replaced, and its own request is checked in their place.
        let through = |site: &Site| {
            let mut via = site.via.clone();
            while let Some(found) = via {
                if Arc::ptr_eq(&found, caller) {
                    return true;
                }
                via = found.site.via.clone();
            }
            false
        };
        self.obligations.retain(|(site, _, _)| !through(site));
        self.guards.retain(|(site, _)| !through(site));
        let key = (caller.site.expr.span.start, caller.site.expr.span.end);
        let editable = self
            .snapshot
            .graph
            .modules
            .iter()
            .find(|m| m.program.canonical.name == caller.site.module)
            .is_some_and(|m| {
                self.snapshot
                    .editable_source(&m.path, caller.site.expr.span)
                    .is_some()
            });
        if editable && !caller.site.locked && !caller.site.synthetic {
            // Edits inside the call, and edits elsewhere that were reached
            // through it (an argument's definition, say).
            let inside = self
                .patches
                .keys()
                .filter(|&&(a, b)| {
                    (key.0 <= a && b <= key.1 && (a, b) != key)
                        || self.patch_via.get(&(a, b)).is_some_and(|via| {
                            let mut via = Some(via.clone());
                            while let Some(found) = via {
                                if Arc::ptr_eq(&found, caller) {
                                    return true;
                                }
                                via = found.site.via.clone();
                            }
                            false
                        })
                })
                .copied()
                .collect::<Vec<_>>();
            for span in inside {
                self.patches.remove(&span);
                self.changed_spans.remove(&span);
                self.patch_locations.remove(&span);
                self.patch_via.remove(&span);
            }
            self.spliced.insert(key);
        }
        self.materialize(&caller.site, &caller.request, &caller.ty)
    }
    fn list(
        &mut self,
        site: &Site,
        request: TypedValue,
        request_type: Ty,
    ) -> Result<(), EditError> {
        let Some(fields) = site.list_fields() else {
            return self.materialize(site, &request, &request_type);
        };
        let ctor = GlobalName {
            module: vec!["Prelude".into()],
            name: ":".into(),
        };
        self.route_constructor(site, ctor, fields.into(), request, request_type)
    }

    fn call(&mut self, s: Site, r: TypedValue, t: Ty) -> Result<(), EditError> {
        let (fun, args) = self.application_parts(&s, "")?;
        let request_name = self.probes.reference(&r)?;
        let call = self.checked_call(&s, fun, args, &t, &request_name, "")?;
        if call.definition.is_none() {
            return self.constructor(&s, call.name, call.args, r, t, call.output);
        }
        let name = call.name.clone();
        if self.callback_factories.iter().any(|p| p.function == name)
            || !call.bodies.is_empty()
            || call.has_companions()
        {
            return self.higher_order_call(
                higher_order::SourceCall {
                    name,
                    args: call.args,
                    actuals: call.actuals,
                    declarations: call.declarations,
                    output: call.output,
                    request_type: t,
                    rules: call.rules.len(),
                    bodies: call.bodies,
                },
                &s,
                &r,
            );
        }
        let selection = if call.rules.is_empty() {
            None
        } else {
            Some(call.select()?)
        };
        let call_frame::CheckedCall {
            fun,
            args,
            rules,
            definition,
            output: output_type,
            ..
        } = call;
        let f = definition.unwrap();
        if selection.is_none() {
            self.retain_argument_pins(&args)?;
            if f.params.len() != args.len() {
                return unsupported("partial applications need callback forwarding");
            }
            let scope = self.scopes.len();
            self.scopes.push(self.scopes[fun.scope].clone());
            let caller = Arc::new(syntax::Caller {
                site: s.clone(),
                request: r.clone(),
                ty: t.clone(),
            });
            for (p, mut a) in f.params.iter().zip(args) {
                // An argument reached through the body belongs to this call.
                a.via = Some(caller.clone());
                self.bind_pattern(&p.node, a, scope, &name.module, false)?;
            }
            return self.schedule(
                Site {
                    recipe: None,
                    candidate_source: false,
                    via: Some(caller),
                    lash: Some(s.lash_root()),
                    synthetic: false,
                    projections: vec![],
                    module: name.module,
                    expr: f.body,
                    scope,
                    locked: fun.locked,
                    fixed: false,
                    ty: Some(output_type),
                },
                r,
                t,
            );
        }
        let (selected, input) = selection.unwrap();
        let expression = self.source(&rules[selected])?;
        // A call that passes its last argument on as the same type of value
        // (a setter in a pipeline) continues that value's chain.
        let mut args = args;
        if let Some(last) = args.last_mut() {
            if last.ty.is_some() && last.ty == Some(output_type.clone()) {
                last.lash = Some(s.lash_root());
            }
        }
        self.apply_rule(&s, &t, args, r, input, &expression)
    }

    fn apply_rule(
        &mut self,
        site: &Site,
        request_type: &Ty,
        args: Vec<Site>,
        r: TypedValue,
        input: Ty,
        expression: &str,
    ) -> Result<(), EditError> {
        for arg in &args {
            self.reject_partial_solver_pin(arg)?;
        }
        self.apply_field_rule(site, request_type, args, r, input, expression)
    }

    // Generated field routes and the standard cons rule ignore feeds and route
    // requests into children, where partial solver pins remain enforceable.
    /// `site` is the call the rule answers for: a `Splice` replaces it.
    fn apply_field_rule(
        &mut self,
        site: &Site,
        request_type: &Ty,
        mut args: Vec<Site>,
        r: TypedValue,
        input: Ty,
        expression: &str,
    ) -> Result<(), EditError> {
        let pins = self.retain_argument_pins(&args)?;
        let mut values = self.source_arguments(&args)?;
        let mut parameters = (0..args.len())
            .map(|i| format!("arg{i}"))
            .collect::<Vec<_>>();
        let mut states = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let pin = if pins[i] {
                let value = self
                    .solver_pin_value(arg)?
                    .unwrap_or_else(|| values[i].clone());
                values.push(value);
                parameters.push(format!("pin{i}"));
                format!("Just pin{i}")
            } else {
                "Nothing".into()
            };
            states.push(format!("{{ value = arg{i}, pinned = {pin} }}"));
        }
        parameters.push("request".into());
        values.push(r.clone());
        let proposal = self.probes.call_typed(
            &format!(
                "\\{} -> ({}) {} request",
                parameters.join(" "),
                expression,
                group(&states)
            ),
            &values,
            &Ty::Named("Unravelled".into(), vec![input.clone()]),
        )?;
        let input_value = match self.probes.unravelled_payload(&proposal, &input)? {
            RuleAnswer::Unravel(value) => value,
            RuleAnswer::Splice => return self.materialize(site, &r, request_type),
            RuleAnswer::Snag => {
                self.note_snag(site, &r);
                return Err(EditError::RuleFailed);
            }
        };
        if args.len() == 1 {
            return self.schedule(args.pop().unwrap(), input_value, input);
        }
        let Ty::Tuple(ts) = input else {
            return Err(EditError::Internal("multiargument rule result".into()));
        };
        for (i, (a, t)) in args.into_iter().zip(ts).enumerate() {
            let r = self.probes.project_live(&input_value, Field::Index(i))?;
            self.schedule(a, r, t)?;
        }
        Ok(())
    }

    /// A materialized value remains editable on subsequent transactions. For
    /// the same constructor, follow its fields rather than replacing aliases or
    /// computations embedded in those fields. A constructor change uses the
    /// existing checked materializer at the original writable expression.
    fn constructor(
        &mut self,
        site: &Site,
        constructor: GlobalName,
        args: Vec<Site>,
        request: TypedValue,
        request_type: Ty,
        output: Ty,
    ) -> Result<(), EditError> {
        let mut site = site.clone();
        site.ty = Some(output);
        self.route_constructor(&site, constructor, args, request, request_type)
    }
    fn route_constructor(
        &mut self,
        site: &Site,
        constructor: GlobalName,
        args: Vec<Site>,
        request: TypedValue,
        request_type: Ty,
    ) -> Result<(), EditError> {
        let types = args
            .iter()
            .map(|a| {
                a.ty.clone().ok_or_else(|| {
                    EditError::Unsupported("constructor source fields need checked types".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.remember_constructor_fields(site, &constructor, &args)?;
        if let Some((expression, input)) =
            self.select_constructor_rule(site, &constructor, &args, &request_type)?
        {
            if expression == "Prelude.unravelListCons"
                && self.try_list_route(site, &request, &request_type)?
            {
                return Ok(());
            }
            return self.apply_rule(site, &request_type, args, request, input, &expression);
        }
        self.retain_argument_pins(&args)?;
        if let Some((expression, input)) =
            self.select_list_rule(site, &constructor, &args, &request_type)?
        {
            if self.try_list_route(site, &request, &request_type)? {
                return Ok(());
            }
            return self.apply_field_rule(site, &request_type, args, request, input, &expression);
        }
        // Default adapter has the declared Maybe Parent contract. Other
        // requests need an explicit rule or instance materialization.
        if request_type != Ty::Named("Maybe".into(), vec![site.ty.clone().unwrap()]) {
            return self.materialize(site, &request, &request_type);
        }
        if args.is_empty() {
            return self.materialize(site, &request, &request_type);
        }
        let expression = constructor_rules::exact_constructor_rule(&constructor, &types);
        let (expression, input) = self
            .select_field_rule(site, &args, &request_type, &[expression])?
            .ok_or(EditError::RuleFailed)?;
        // The exact default answers Splice for a different variant, like any
        // rule that cannot express a request through its arguments.
        self.apply_field_rule(site, &request_type, args, request, input, &expression)
    }
}

fn group(xs: &[String]) -> String {
    if xs.len() == 1 {
        xs[0].clone()
    } else {
        format!("({})", xs.join(", "))
    }
}
fn flatten(s: &Site) -> (Site, Vec<Site>) {
    match &s.expr.node {
        CExpr::App(f, a) => {
            let (f, mut args) = flatten(&s.child(f));
            args.push(s.child(a));
            (f, args)
        }
        CExpr::BinOp(r, a, b) => (
            Site {
                expr: Spanned::new(s.expr.span, CExpr::Var(r.clone())),
                ..s.clone()
            },
            vec![s.child(a), s.child(b)],
        ),
        _ => (s.clone(), vec![]),
    }
}
