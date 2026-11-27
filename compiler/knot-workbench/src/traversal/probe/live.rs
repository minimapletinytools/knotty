//! Transaction-owned requests. Checked query lambdas take retained values as
//! actual arguments; stubs exist only for metadata checking, never execution.
use super::*;
use crate::operation::{OperationSession, PreparedOperations};
use knot_core::{link::RevisionOverlay, Field};

pub(super) mod route;

pub(super) struct Live {
    prepared: PreparedOperations,
    session: OperationSession,
    sequence: usize,
    source_bytes: usize,
    charged: crate::unravel::EditUsage,
    revision: Option<RevisionOverlay>,
    revisions: usize,
    operations: BTreeMap<String, TypedValue>,
}
impl Live {
    fn new(
        prepared: PreparedOperations,
        session: OperationSession,
        sequence: usize,
        source_bytes: usize,
    ) -> Self {
        Self {
            prepared,
            session,
            sequence,
            source_bytes,
            charged: Default::default(),
            revision: None,
            revisions: 0,
            operations: Default::default(),
        }
    }
}
impl Ty {
    pub(in crate::traversal) fn from_value(value: &ValueType) -> Self {
        match value {
            ValueType::Named(name, args) => Self::Named(
                if name.module == ["Prelude"] {
                    name.name.clone()
                } else {
                    format!("{}.{}", name.module.join("."), name.name)
                },
                args.iter().map(Self::from_value).collect(),
            ),
            ValueType::Function(a, b) => {
                Self::Function(Box::new(Self::from_value(a)), Box::new(Self::from_value(b)))
            }
            ValueType::Tuple(fields) => Self::Tuple(fields.iter().map(Self::from_value).collect()),
            ValueType::Record(fields) => Self::Record(
                fields
                    .iter()
                    .map(|(n, t)| (n.clone(), Self::from_value(t)))
                    .collect(),
            ),
            ValueType::Unit => Self::Unit,
        }
    }
}
impl Probes {
    pub(in crate::traversal) fn observed_data_key(
        &mut self,
        value: &TypedValue,
    ) -> Result<Option<twine::ObservedDataKey>, EditError> {
        self.with_live(|session| session.observed_data_key(value).map_err(operation_error))
    }
    /// Prepare a closed callable once per concrete signature/candidate revision.
    /// Actual arguments (including lazy fields and captured request closures) are
    /// passed as typed handles, never embedded in a newly checked application.
    pub(in crate::traversal) fn call_typed(
        &mut self,
        function: &str,
        arguments: &[TypedValue],
        output: &Ty,
    ) -> Result<TypedValue, EditError> {
        let started = Instant::now();
        if self
            .limits
            .runtime
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        let signature = arguments.iter().rev().fold(output.clone(), |rest, value| {
            Ty::Function(Box::new(Ty::from_value(value.value_type())), Box::new(rest))
        });
        // This namespace cannot collide with pool::key's length-prefixed keys.
        let key = format!("typed:{}:{function}", signature.source());
        if key.len() > self.limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        let cached = self
            .live
            .as_ref()
            .and_then(|live| live.operations.get(&key).cloned());
        self.check_ms += started.elapsed().as_secs_f64() * 1000.;
        let callable = if let Some(callable) = cached {
            // No source query is built/checked on this path. Like project_live,
            // direct application is charged through the cumulative runtime
            // budget below, not as another checked-source query.
            callable
        } else {
            // The expression must be closed over imported globals, not ambient
            // query helpers. Revision installation clears this operation pool.
            self.declarations.clear();
            let (callable, _) = self.evaluate_lazy(function, Some(&signature))?;
            self.live
                .as_mut()
                .unwrap()
                .operations
                .insert(key, callable.clone());
            callable
        };
        self.with_live(|session| session.apply(&callable, arguments).map_err(operation_error))
    }

    pub(in crate::traversal) fn reference(
        &mut self,
        value: &TypedValue,
    ) -> Result<String, EditError> {
        if self.references.len() >= self.limits.traversal.retained_values {
            return unsupported("transaction retained-value limit exceeded");
        }
        let name = format!("{}{}_", self.context.value_prefix, self.references.len());
        self.references.insert(name.clone(), value.clone());
        Ok(name)
    }
    pub(in crate::traversal) fn referenced(&self, source: &str) -> Vec<(String, TypedValue)> {
        self.references
            .iter()
            .filter(|(name, _)| source.contains(name.as_str()))
            .map(|(n, v)| (n.clone(), v.clone()))
            .collect()
    }
    pub(in crate::traversal) fn evaluate_lazy(
        &mut self,
        expression: &str,
        ty: Option<&Ty>,
    ) -> Result<(TypedValue, Ty), EditError> {
        self.execute_lazy(
            &format!(
                "{}probe =\n{}\n",
                ty.map(|t| format!("probe :: {}\n", t.source()))
                    .unwrap_or_default(),
                indent(expression)
            ),
            &[],
        )
    }
    pub(in crate::traversal) fn evaluate_lazy_call(
        &mut self,
        function: &str,
        arguments: &[(&str, Ty)],
        output: &Ty,
    ) -> Result<(TypedValue, Ty), EditError> {
        let signature = arguments.iter().rev().fold(output.clone(), |out, (_, ty)| {
            Ty::Function(Box::new(ty.clone()), Box::new(out))
        });
        self.execute_lazy(
            &format!(
                "probe :: {}\nprobe =\n{}\n",
                signature.source(),
                indent(function)
            ),
            &arguments.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        )
    }
    pub(in crate::traversal) fn execute_lazy(
        &mut self,
        body: &str,
        arguments: &[&str],
    ) -> Result<(TypedValue, Ty), EditError> {
        let started = Instant::now();
        if self.validating {
            self.validations.pending = None;
        }
        if self
            .limits
            .runtime
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        self.count += 1;
        if self.count > self.limits.traversal.queries {
            return unsupported("source traversal exceeded its checked-query budget");
        }
        let contents = format!("{}\n{body}", self.declarations);
        let references = self.referenced(&contents);
        let sequence = self.live.as_ref().map_or(0, |l| l.sequence);
        let module_name = format!("TwineTraversalLive{sequence}");
        if self
            .context
            .imports
            .interfaces()
            .iter()
            .any(|i| i.name == [module_name.clone()])
        {
            return unsupported("reserved live query namespace collision");
        }
        let name = format!("twineLiveOperation{sequence}");
        let bindings = references
            .iter()
            .map(|(n, v)| {
                format!(
                    "{n} :: {}\n{n} = undefined\n",
                    Ty::from_value(v.value_type()).source()
                )
            })
            .collect::<String>();
        let source = format!(
            "{}\n{bindings}{contents}\n{name} = probe {}\n",
            self.context
                .header
                .replacen("TwineTraversalQuery", &module_name, 1),
            arguments.join(" ")
        );
        let inputs = references
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>();
        if source.len() > self.limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        // Only the outer synthetic module/export names vary between installs.
        // Include stub boundaries and the complete typed source and argument order.
        let lookup = (!self.validating && (self.route_image.is_some() || self.recording.is_some()))
            .then(|| {
                format!(
                    "{}:{bindings}{contents}\n$entry = probe {}",
                    bindings.len(),
                    arguments.join(" ")
                )
            });
        if let Some((label, normalized)) = lookup.as_deref().and_then(|k| self.route_operation(k)) {
            let live = self.live.as_mut().unwrap();
            let callable = live.session.binding(&label).map_err(operation_error)?;
            if normalized
                .as_ref()
                .is_none_or(|key| !live.operations.contains_key(key))
            {
                if live.source_bytes.saturating_add(source.len())
                    > self.limits.traversal.compiled_bytes
                {
                    return Err(EditError::SourceTooLarge);
                }
                live.source_bytes += source.len();
            }
            if let Some(key) = normalized {
                live.operations.insert(key, callable.clone());
            }
            self.check_ms += started.elapsed().as_secs_f64() * 1000.;
            self.cache_hits += 1;
            self.route_hits += 1;
            let result = self.with_live(|s| {
                s.apply(
                    &callable,
                    &references
                        .iter()
                        .map(|(_, v)| v.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(operation_error)
            })?;
            let ty = Ty::from_value(result.value_type());
            return Ok((result, ty));
        }
        let cache_key = format!("live:{source}");
        let cached = {
            let mut cache = self.context.checked.lock().unwrap();
            cache
                .iter()
                .position(|(key, _)| key == &cache_key)
                .map(|index| {
                    let entry = cache.remove(index).unwrap();
                    let checked = entry.1.program.clone();
                    cache.push_back(entry);
                    checked
                })
        };
        let parsed = if cached.is_none() {
            let raw = knot_syntax::parse(&source)
                .map_err(|e| EditError::Parse(format!("{}\n{source}", e.render(&source))))?;
            Some(
                knot_canonical::canonicalize_module(
                    &raw,
                    Some(&Registry(self.context.imports.interfaces())),
                )
                .map_err(|e| EditError::Check(format!("{e:?}\n{source}")))?,
            )
        } else {
            None
        };
        let canonical = cached
            .as_ref()
            .map(|p| &p.canonical)
            .or(parsed.as_ref())
            .unwrap();
        let operation_key = pool::key(&source, canonical, &references, &module_name, &name);
        if self.validating {
            if let Some(value) = operation_key.as_ref().and_then(|key| {
                self.validations.reuse(
                    key,
                    &references,
                    self.context.base.shared_program(),
                    self.limits.traversal.dependency_nodes,
                )
            }) {
                if self
                    .limits
                    .runtime
                    .cancellation
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return Err(twine::Outcome::Cancelled.into());
                }
                self.check_ms += started.elapsed().as_secs_f64() * 1000.;
                let ty = Ty::from_value(value.value_type());
                return Ok((value, ty));
            }
        }
        let reused = operation_key
            .as_ref()
            .and_then(|key| self.live.as_ref()?.operations.get(key).cloned());
        if let Some(function) = reused {
            self.cache_hits += 1;
            self.check_ms += started.elapsed().as_secs_f64() * 1000.;
            let result = self.with_live(|session| {
                session
                    .apply(
                        &function,
                        &references
                            .iter()
                            .map(|(_, v)| v.clone())
                            .collect::<Vec<_>>(),
                    )
                    .map_err(operation_error)
            })?;
            let ty = Ty::from_value(result.value_type());
            if self.validating {
                if let Some(key) = &operation_key {
                    if let Some(proof) = self.validations.proof(key) {
                        self.validations.stage(
                            key.clone(),
                            proof,
                            &references,
                            &result,
                            self.context.base.shared_program(),
                        );
                    }
                }
            }
            return Ok((result, ty));
        }
        let retained = self.live.as_ref().map_or(0, |l| l.source_bytes);
        if source.len() > self.limits.source_bytes
            || retained.saturating_add(source.len()) > self.limits.traversal.compiled_bytes
        {
            return Err(EditError::SourceTooLarge);
        }
        // Compiled-query reuse is separate from value reuse. The exact source
        // includes input types and scalar literals; handles are supplied afresh.
        let checked = if let Some(checked) = &cached {
            self.cache_hits += 1;
            checked.clone()
        } else {
            let checked = Arc::new(
                self.context
                    .imports
                    .check(canonical, &Default::default())
                    .map_err(|e| EditError::Check(format!("{e:?}\n{source}")))?,
            );
            let mut cache = self.context.checked.lock().unwrap();
            cache.push_back((
                cache_key,
                Arc::new(CheckedProbe {
                    program: checked.clone(),
                    source: String::new(),
                    frame: Default::default(),
                }),
            ));
            while cache.len() > 256
                || cache
                    .iter()
                    .map(|(source, p)| source.len() + p.source.len())
                    .sum::<usize>()
                    > 4 * 1024 * 1024
            {
                cache.pop_front();
            }
            checked
        };
        self.check_ms += started.elapsed().as_secs_f64() * 1000.;
        let started = Instant::now();
        let run_before_link = self.run_ms;
        if let Some(live) = &self.live {
            let next = live
                .prepared
                .extend_parameterized(&checked, &inputs, &name, live.revision.as_ref())
                .map_err(EditError::Check)?;
            self.with_live(|session| session.install(&next).map_err(operation_error))?;
            if let Some(key) = lookup {
                self.record_operation(key, &name, operation_key.as_ref(), source.len());
            }
            let live = self.live.as_mut().unwrap();
            live.prepared = next;
            live.sequence += 1;
            live.source_bytes += source.len();
        } else {
            let prepared =
                PreparedOperations::parameterized(&self.context.base, &checked, &inputs, &name)
                    .map_err(EditError::Check)?;
            let mut limits = self.limits.runtime.clone();
            limits.steps = limits.steps.saturating_sub(self.usage.steps);
            limits.heap_entries = limits.heap_entries.saturating_sub(self.usage.heap_entries);
            #[cfg(feature = "incremental")]
            let inherited = self.start_from_baseline(&prepared, &mut limits)?;
            #[cfg(not(feature = "incremental"))]
            let inherited = None;
            let session = match inherited {
                Some(session) => session,
                None => prepared.start(limits).map_err(operation_error)?,
            };
            self.live = Some(Live::new(prepared, session, 1, source.len()));
        }
        if let Some(key) = &operation_key {
            let live = self.live.as_mut().unwrap();
            let function = live.session.binding(&name).map_err(operation_error)?;
            live.operations.insert(key.clone(), function);
        }
        self.link_ms +=
            (started.elapsed().as_secs_f64() * 1000. - (self.run_ms - run_before_link)).max(0.);
        let result = self.with_live(|session| {
            let function = session.binding(&name).map_err(operation_error)?;
            session
                .apply(
                    &function,
                    &references
                        .iter()
                        .map(|(_, v)| v.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(operation_error)
        })?;
        let ty = Ty::from_value(result.value_type());
        if self.validating {
            if let Some(key) = operation_key {
                let proof = self
                    .validations
                    .proof(&key)
                    .unwrap_or_else(|| super::validation::Proof::new(checked, &name, &inputs));
                self.validations.stage(
                    key,
                    proof,
                    &references,
                    &result,
                    self.context.base.shared_program(),
                );
            }
        }
        Ok((result, ty))
    }
    fn with_live<T>(
        &mut self,
        f: impl FnOnce(&mut OperationSession) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let started = Instant::now();
        let live = self
            .live
            .as_mut()
            .ok_or_else(|| EditError::Internal("no live query session".into()))?;
        let remaining_steps = self.limits.runtime.steps.saturating_sub(self.usage.steps);
        let remaining_heap = self
            .limits
            .runtime
            .heap_entries
            .saturating_sub(self.usage.heap_entries);
        live.session.restrict_execution(
            live.charged.steps.saturating_add(remaining_steps),
            live.charged.heap_entries.saturating_add(remaining_heap),
        );
        let result = f(&mut live.session);
        let usage = crate::unravel::EditUsage {
            steps: live.session.stats().steps,
            heap_entries: live.session.heap_entries(),
        };
        self.usage.steps += usage.steps.saturating_sub(live.charged.steps);
        self.usage.heap_entries += usage.heap_entries.saturating_sub(live.charged.heap_entries);
        live.charged = usage;
        self.run_ms += started.elapsed().as_secs_f64() * 1000.;
        result
    }
    pub(in crate::traversal) fn project_live(
        &mut self,
        value: &TypedValue,
        field: Field,
    ) -> Result<TypedValue, EditError> {
        self.with_live(|session| session.project(value, &field).map_err(operation_error))
    }
    pub(in crate::traversal) fn has_structural_fields(
        &mut self,
        value: &TypedValue,
    ) -> Result<bool, EditError> {
        self.with_live(|session| {
            let shell = session.force(value).map_err(operation_error)?;
            Ok(match session.view(shell).map_err(operation_error)? {
                ValueView::Constructor(_, fields) | ValueView::Tuple(fields) => fields > 0,
                ValueView::Record(fields) => !fields.is_empty(),
                _ => false,
            })
        })
    }
    pub(in crate::traversal) fn constructor_live(
        &mut self,
        value: &TypedValue,
    ) -> Result<(GlobalName, usize), EditError> {
        self.with_live(|session| {
            let shell = session.force(value).map_err(operation_error)?;
            match session.view(shell).map_err(operation_error)? {
                ValueView::Constructor(name, arity) => Ok((name.clone(), arity)),
                _ => Err(EditError::Internal(
                    "request has no constructor shell".into(),
                )),
            }
        })
    }
    pub(in crate::traversal) fn maybe_payload(
        &mut self,
        value: &TypedValue,
        payload: &Ty,
    ) -> Result<Option<TypedValue>, EditError> {
        let (name, arity) = self.constructor_live(value)?;
        if name.module == ["Prelude"] && name.name == "Nothing" && arity == 0 {
            return Ok(None);
        }
        if name.module != ["Prelude"] || name.name != "Just" || arity != 1 {
            return Err(EditError::Internal("expected Maybe proposal".into()));
        }
        let declarations = std::mem::take(&mut self.declarations);
        let result = self.call_typed(
            "\\proposal -> case proposal of\n  Just value -> value\n  Nothing -> error \"missing proposal\"",
            std::slice::from_ref(value),
            payload,
        );
        self.declarations = declarations;
        result.map(Some)
    }
    /// Read a rule's answer. `Splice` and `Snag` carry nothing.
    pub(in crate::traversal) fn unravelled_payload(
        &mut self,
        value: &TypedValue,
        payload: &Ty,
    ) -> Result<RuleAnswer, EditError> {
        let (name, arity) = self.constructor_live(value)?;
        if name.module != ["Prelude"] {
            return Err(EditError::Internal("expected Unravelled answer".into()));
        }
        match (name.name.as_str(), arity) {
            ("Snag", 0) => return Ok(RuleAnswer::Snag),
            ("Splice", 0) => return Ok(RuleAnswer::Splice),
            ("Unravel", 1) => {}
            _ => return Err(EditError::Internal("expected Unravelled answer".into())),
        }
        let declarations = std::mem::take(&mut self.declarations);
        let result = self.call_typed(
            "\\answer -> case answer of\n  Unravel value -> value\n  _ -> error \"missing answer\"",
            std::slice::from_ref(value),
            payload,
        );
        self.declarations = declarations;
        result.map(RuleAnswer::Unravel)
    }
    pub(in crate::traversal) fn decode_live(
        &mut self,
        value: &TypedValue,
    ) -> Result<Data, EditError> {
        self.with_live(|session| {
            let shell = session.force(value).map_err(operation_error)?;
            decode(session.runtime_mut(), shell, 0, &mut 0)
        })
    }
    pub(in crate::traversal) fn materialize_live(
        &mut self,
        value: &TypedValue,
        module: &knot_canonical::ast::CModule,
    ) -> Result<materialization::Materialized, EditError> {
        let mut remaining = self
            .limits
            .traversal
            .materialized_values
            .saturating_sub(self.materialized_values);
        let before = remaining;
        let bytes = self.limits.source_bytes;
        let result = self.with_live(|session| {
            let root = session.force(value).map_err(operation_error)?;
            materialization::read(session.runtime_mut(), root, module, &mut remaining, bytes)
        });
        self.materialized_values += before - remaining;
        result
    }
    pub(in crate::traversal) fn install_candidate(
        &mut self,
        original: &ProjectSnapshot,
        candidate: &ProjectSnapshot,
        libraries: &knot_core::Program,
    ) -> Result<(), EditError> {
        self.finish_recording();
        self.route_image = None;
        let live = self
            .live
            .as_ref()
            .ok_or_else(|| EditError::Internal("candidate has no request session".into()))?;
        if self
            .limits
            .runtime
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        // Returning to the exact original snapshot selects its existing code;
        // it does not erase old candidates, handles, counters or consumed work.
        if candidate.id() == original.id() {
            let live = self.live.as_mut().unwrap();
            live.revision = None;
            live.operations.clear();
            self.context = self.original_context.clone();
            return Ok(());
        }
        if live.revisions >= self.limits.traversal.candidate_revisions {
            return unsupported("transaction candidate revision limit exceeded");
        }
        let candidate_bytes = candidate.sources().values().map(String::len).sum::<usize>();
        if live.source_bytes.saturating_add(candidate_bytes) > self.limits.traversal.compiled_bytes
        {
            return Err(EditError::SourceTooLarge);
        }
        let context = Arc::new(ProbeContext::new_reusing(
            candidate,
            libraries,
            Some(&self.original_context),
        )?);
        let (next, revision) = live
            .prepared
            .candidate_revision(
                &original.graph.entry().program,
                &candidate.graph.entry().program,
                vec![format!("TwineTraversalCandidate{}", live.revisions)],
                &[],
            )
            .map_err(EditError::Check)?;
        self.with_live(|session| session.install(&next).map_err(operation_error))?;
        let live = self.live.as_mut().unwrap();
        live.prepared = next;
        live.revision = Some(revision);
        live.revisions += 1;
        live.operations.clear();
        live.source_bytes += candidate_bytes;
        self.context = context;
        Ok(())
    }
}

#[cfg(test)]
mod revision_tests;

#[cfg(all(test, feature = "incremental"))]
mod baseline_tests;
