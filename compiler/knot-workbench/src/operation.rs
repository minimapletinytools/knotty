//! Checked, reusable calls over session-owned lazy values. A value's type comes
//! from a prepared binding or a checked application, never from a runtime cast.
//! Specialize polymorphic exports in Knot before preparing them; their selected
//! dictionaries then travel in ordinary checked core and closure environments.
use knot_checker::{
    program::{BindingInfo, GlobalName, NodeRef, TypedProgram},
    ty::Structure,
    var::TypeVarId,
};
use knot_core::{
    link::{LinkedProgram, ProgramExtension, RevisionOverlay},
    Field, Program,
};
use std::{collections::BTreeMap, sync::Arc};
use twine::{HostRegistry, Limits, Outcome, Session, Stats, ThunkHandle, ValueHandle, ValueView};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueType {
    Named(GlobalName, Vec<ValueType>),
    Function(Box<ValueType>, Box<ValueType>),
    Tuple(Vec<ValueType>),
    Record(BTreeMap<String, ValueType>),
    Unit,
}
impl ValueType {
    pub fn of_binding(module: &TypedProgram, name: &str) -> Result<Self, String> {
        let def = module
            .definition(name)
            .ok_or_else(|| format!("missing checked binding {name}"))?;
        Self::of_export(module, &module.bindings[&def.binding])
    }

    fn of_export(module: &TypedProgram, binding: &BindingInfo) -> Result<Self, String> {
        if !binding.parameters.is_empty()
            || !binding.scheme.vars.is_empty()
            || !binding.scheme.constraints.is_empty()
        {
            return Err(format!(
                "specialize operation binding {} before preparing it",
                binding.name
            ));
        }
        Self::read(module, binding.scheme.ty)
    }

    pub(crate) fn read(p: &TypedProgram, id: TypeVarId) -> Result<Self, String> {
        Ok(match p.substitution.head_normal_readonly(id) {
            Some(Structure::App(name, args)) => Self::Named(
                p.qualify(&name).ok_or("unqualified operation type")?,
                args.into_iter()
                    .map(|t| Self::read(p, t))
                    .collect::<Result<_, _>>()?,
            ),
            Some(Structure::Fn(a, b)) => {
                Self::Function(Box::new(Self::read(p, a)?), Box::new(Self::read(p, b)?))
            }
            Some(Structure::Tuple(ts)) => Self::Tuple(
                ts.into_iter()
                    .map(|t| Self::read(p, t))
                    .collect::<Result<_, _>>()?,
            ),
            Some(Structure::Record(mut fields, mut extension)) => {
                while let Some(row) = extension {
                    let Some(Structure::Record(more, rest)) =
                        p.substitution.head_normal_readonly(row)
                    else {
                        return Err("operation types must have closed record rows".into());
                    };
                    fields.extend(more);
                    extension = rest;
                }
                Self::Record(
                    fields
                        .into_iter()
                        .map(|(n, t)| Ok((n, Self::read(p, t)?)))
                        .collect::<Result<_, String>>()?,
                )
            }
            Some(Structure::Unit) => Self::Unit,
            _ => return Err("operation types must be fully specialized".into()),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    ArgumentCount {
        expected: usize,
        actual: usize,
    },
    UnknownBinding(String),
    Extension(twine::ExtensionError),
    InvalidField(Field),
    ValueShape,
    NotAFunction,
    ArgumentType {
        index: usize,
        expected: Box<ValueType>,
        actual: Box<ValueType>,
    },
    Runtime(Outcome),
    HostLink(String),
}
impl From<Outcome> for CallError {
    fn from(value: Outcome) -> Self {
        Self::Runtime(value)
    }
}

#[derive(Clone)]
struct Export {
    name: GlobalName,
    ty: ValueType,
    origin: NodeRef,
}
// Export descriptors never mutate after preparation. Extending the label map
// shares their potentially large concrete types with earlier preparations.
type Exports = BTreeMap<String, Arc<Export>>;
/// Immutable checked exports and the executable program they were lowered into.
/// Preparing does not evaluate any binding. Imports remain in the linked base.
#[derive(Clone)]
pub struct PreparedOperations {
    linked: Option<LinkedProgram>,
    extension: Option<ProgramExtension>,
    program: Arc<Program>,
    exports: Arc<Exports>,
    #[cfg(feature = "incremental")]
    source_context: twine::RevisionContext,
}
impl PreparedOperations {
    pub fn shared_program(&self) -> &Arc<Program> {
        &self.program
    }

    pub fn link(
        base: &LinkedProgram,
        module: &TypedProgram,
        exports: &[&str],
    ) -> Result<Self, String> {
        let linked = base.extend(&[module])?;
        let mut prepared = Self::from_linked(linked.shared_program(), module, exports)?;
        prepared.linked = Some(linked);
        Ok(prepared)
    }
    // Only checked linking uses this seam. Public callers cannot pair unrelated
    // types and executable code.
    pub(crate) fn from_linked(
        program: Arc<Program>,
        module: &TypedProgram,
        names: &[&str],
    ) -> Result<Self, String> {
        let mut exports = BTreeMap::new();
        for name in names {
            let def = module
                .definition(name)
                .ok_or_else(|| format!("no operation binding {name}"))?;
            let binding = &module.bindings[&def.binding];
            exports.insert(
                (*name).into(),
                Arc::new(Export {
                    name: binding
                        .global
                        .clone()
                        .ok_or("operation export must be global")?,
                    ty: ValueType::of_export(module, binding)?,
                    origin: module.node_ref(def.body.id),
                }),
            );
        }
        Ok(Self {
            linked: None,
            extension: None,
            program,
            exports: Arc::new(exports),
            #[cfg(feature = "incremental")]
            source_context: None,
        })
    }
    /// Prepare additional monomorphic operations over the same immutable code.
    /// Export labels must be distinct; previous bindings and handles stay usable.
    pub fn extend(&self, module: &TypedProgram, exports: &[&str]) -> Result<Self, String> {
        let linked = self
            .linked
            .as_ref()
            .ok_or("this prepared query has no append lineage")?;
        let (next, extension) = linked.extension(&[module])?;
        let mut prepared = Self::from_linked(next.shared_program(), module, exports)?;
        let mut combined = self.exports.as_ref().clone();
        for (name, export) in prepared.exports.iter() {
            if combined.insert(name.clone(), export.clone()).is_some() {
                return Err(format!("duplicate operation export label {name}"));
            }
        }
        prepared.exports = Arc::new(combined);
        prepared.linked = Some(next);
        prepared.extension = Some(extension);
        Ok(prepared)
    }
    /// Close a fresh checked value-only module over selected input bindings.
    /// Input stubs are replaced by actual typed arguments in a recursive let;
    /// neither their bodies nor a source serialization of the values executes.
    pub fn parameterized(
        base: &LinkedProgram,
        module: &TypedProgram,
        inputs: &[&str],
        output: &str,
    ) -> Result<Self, String> {
        Self::parameterized_on(base, module, inputs, output, None, &BTreeMap::new())
    }
    pub fn extend_parameterized(
        &self,
        module: &TypedProgram,
        inputs: &[&str],
        output: &str,
        revision: Option<&RevisionOverlay>,
    ) -> Result<Self, String> {
        Self::parameterized_on(
            self.linked
                .as_ref()
                .ok_or("this query has no checked append lineage")?,
            module,
            inputs,
            output,
            revision,
            &self.exports,
        )
    }
    fn parameterized_on(
        base: &LinkedProgram,
        module: &TypedProgram,
        inputs: &[&str],
        output: &str,
        revision: Option<&RevisionOverlay>,
        existing: &Exports,
    ) -> Result<Self, String> {
        let names = inputs
            .iter()
            .copied()
            .chain(std::iter::once(output))
            .collect::<Vec<_>>();
        let descriptors = Self::from_linked(base.shared_program(), module, &names)?;
        let mut export = descriptors.exports[output].as_ref().clone();
        for input in inputs.iter().rev() {
            export.ty = ValueType::Function(
                Box::new(descriptors.exports[*input].ty.clone()),
                Box::new(export.ty),
            );
        }
        let (next, extension, entry) =
            base.parameterized_extension(module, inputs, output, revision)?;
        export.name = entry;
        let mut exports = existing.clone();
        if exports.insert(output.into(), Arc::new(export)).is_some() {
            return Err(format!("duplicate operation export label {output}"));
        }
        Ok(Self {
            program: next.shared_program(),
            linked: Some(next),
            extension: Some(extension),
            exports: Arc::new(exports),
            #[cfg(feature = "incremental")]
            source_context: None,
        })
    }
    /// Add candidate code with the same nominal/value/instance contracts. Old
    /// exports stay bound to old code; new exports have explicit caller labels.
    pub fn candidate_revision(
        &self,
        original: &TypedProgram,
        candidate: &TypedProgram,
        namespace: Vec<String>,
        exports: &[(&str, &str)],
    ) -> Result<(Self, RevisionOverlay), String> {
        let linked = self
            .linked
            .as_ref()
            .ok_or("this query has no checked append lineage")?;
        let (next, extension, overlay) =
            linked.candidate_revision(original, candidate, namespace)?;
        let names = exports.iter().map(|(_, name)| *name).collect::<Vec<_>>();
        let candidate_exports = Self::from_linked(next.shared_program(), candidate, &names)?;
        let mut combined = self.exports.as_ref().clone();
        for (label, name) in exports {
            let mut export = candidate_exports.exports[*name].as_ref().clone();
            export.name = overlay
                .value(&export.name)
                .ok_or("missing candidate executable binding")?;
            if combined.insert((*label).into(), Arc::new(export)).is_some() {
                return Err(format!("duplicate operation export label {label}"));
            }
        }
        Ok((
            Self {
                linked: Some(next.clone()),
                extension: Some(extension),
                program: next.shared_program(),
                exports: Arc::new(combined),
                #[cfg(feature = "incremental")]
                source_context: None,
            },
            overlay,
        ))
    }
    /// Query candidate values/dictionaries while retaining their original type
    /// identities. The overlay must belong to this linked program's lineage.
    pub fn extend_for_revision(
        &self,
        module: &TypedProgram,
        exports: &[&str],
        overlay: &RevisionOverlay,
    ) -> Result<Self, String> {
        let linked = self
            .linked
            .as_ref()
            .ok_or("this query has no checked append lineage")?;
        let (next, extension) = linked.extension_for_revision(&[module], overlay)?;
        let mut prepared = Self::from_linked(next.shared_program(), module, exports)?;
        let mut combined = self.exports.as_ref().clone();
        for (name, export) in prepared.exports.iter() {
            if combined.insert(name.clone(), export.clone()).is_some() {
                return Err(format!("duplicate operation export label {name}"));
            }
        }
        prepared.exports = Arc::new(combined);
        prepared.linked = Some(next);
        prepared.extension = Some(extension);
        Ok(prepared)
    }
    pub fn start(&self, limits: Limits) -> Result<OperationSession, CallError> {
        self.start_with_hosts(limits, Arc::new(HostRegistry::default()))
    }
    /// Fork an exact checked baseline and install this direct extension. The
    /// baseline is never mutated. A bounded miss returns the consumed work;
    /// callers must deduct it before any fresh fallback. Wrong lineage and
    /// cancellation are errors, not permission to adopt foreign typed values.
    #[cfg(feature = "incremental")]
    pub fn try_start_from(
        &self,
        previous: &OperationSession,
        limits: Limits,
    ) -> Result<OperationForkAttempt, CallError> {
        if self.extension.is_none() {
            return Err(CallError::Extension(twine::ExtensionError::WrongBase));
        }
        let attempt = previous.try_fork(limits)?;
        let Some(mut session) = attempt.session else {
            return Ok(attempt);
        };
        match session.install(self) {
            Ok(()) => Ok(OperationForkAttempt {
                work: session.stats().steps,
                session: Some(session),
            }),
            Err(CallError::Extension(twine::ExtensionError::Runtime(Outcome::BudgetExhausted))) => {
                Ok(OperationForkAttempt {
                    work: session.stats().steps,
                    session: None,
                })
            }
            Err(error) => Err(error),
        }
    }
    /// Certify source reuse from the checked modules behind this exact prepared
    /// program. Callers cannot provide arbitrary ABI bytes or retag raw values.
    /// This is compilation metadata; preparing it does not execute Knot code.
    #[cfg(feature = "incremental")]
    pub fn with_source_context(
        mut self,
        modules: &[&TypedProgram],
        max_bytes: usize,
    ) -> Result<Self, String> {
        self.source_context = Some(crate::revision::runtime_context_for(
            &self.program,
            modules,
            max_bytes,
        )?);
        Ok(self)
    }

    /// Start this checked revision through the same preparation/rebase boundary
    /// as RevisionStore. Missing or unequal checked contexts start fresh. Old
    /// typed values remain foreign unless an exact snapshot granted inheritance.
    #[cfg(feature = "incremental")]
    pub fn start_reusing(
        &self,
        previous: &OperationSession,
        limits: Limits,
        hosts: Arc<HostRegistry>,
        reuse: twine::RevisionReuse,
    ) -> Result<(OperationSession, twine::ReuseStats), CallError> {
        let prepared = previous
            .runtime
            .prepare_revision(
                self.program.clone(),
                (&previous.source_context, &self.source_context),
                limits,
                hosts,
                reuse,
            )
            .map_err(|error| match error {
                twine::RevisionError::Evaluation(error) => CallError::Runtime(error),
                twine::RevisionError::Host(error) => CallError::HostLink(format!("{error:?}")),
                twine::RevisionError::Interrupted | twine::RevisionError::Capacity => {
                    CallError::Runtime(Outcome::BudgetExhausted)
                }
                other => CallError::HostLink(format!("revision preparation: {other:?}")),
            })?;
        Ok((
            OperationSession {
                runtime: prepared.session,
                exports: self.exports.clone(),
                source_context: self.source_context.clone(),
                inheritance: prepared.inheritance,
            },
            prepared.reused,
        ))
    }
    pub fn start_with_hosts(
        &self,
        limits: Limits,
        hosts: Arc<HostRegistry>,
    ) -> Result<OperationSession, CallError> {
        let runtime = Session::with_hosts(self.program.clone(), limits, hosts)
            .map_err(|e| CallError::HostLink(format!("{e:?}")))?;
        Ok(OperationSession {
            runtime,
            exports: self.exports.clone(),
            #[cfg(feature = "incremental")]
            inheritance: None,
            #[cfg(feature = "incremental")]
            source_context: self.source_context.clone(),
        })
    }
}

/// An unforgeable typed thunk, valid only in the session that issued it. No
/// unchecked handle import/retag API is provided. Snapshot inheritance needs an
/// explicit snapshot proof. Cloning preserves sharing, not evaluation.
#[derive(Clone, Debug)]
pub struct TypedValue {
    handle: ThunkHandle,
    ty: ValueType,
    origin: NodeRef,
}
impl TypedValue {
    pub(crate) fn same_thunk(&self, other: &Self) -> bool {
        self.handle == other.handle && self.ty == other.ty
    }
    pub fn value_type(&self) -> &ValueType {
        &self.ty
    }
    /// Origin of the prepared callable/binding; not a cross-edit activation ID.
    pub fn origin(&self) -> NodeRef {
        self.origin
    }
}

pub struct OperationSession {
    runtime: Session,
    exports: Arc<Exports>,
    #[cfg(feature = "incremental")]
    inheritance: Option<twine::SnapshotInheritance>,
    #[cfg(feature = "incremental")]
    source_context: twine::RevisionContext,
}
#[cfg(feature = "incremental")]
pub struct OperationForkAttempt {
    pub work: u64,
    pub session: Option<OperationSession>,
}
impl OperationSession {
    /// Collect a new exact-code/world operation session. Explicit typed roots
    /// retain their lazy values and source scopes; optional runtime retention
    /// uses the same collector. Inherit roots in the returned session before
    /// using them there. Existing handles remain valid only in their old owner.
    /// Prepared code/exports stay shared; this does not reclaim executable code.
    #[cfg(feature = "incremental")]
    pub fn collect_snapshot(
        &self,
        roots: &[TypedValue],
        limits: Limits,
        budget: twine::CollectionBudget,
    ) -> Result<OperationForkAttempt, CallError> {
        let roots = roots
            .iter()
            .map(|value| twine::RetainedRoot::Thunk(value.handle))
            .collect::<Vec<_>>();
        self.collect_runtime_roots(&roots, limits, budget)
    }
    /// Preserve named checked globals, including polymorphic definitions whose
    /// monomorphic uses are selected by later checked queries. This produces no
    /// TypedValue or unchecked type association for those names.
    #[cfg(feature = "incremental")]
    pub fn collect_globals(
        &self,
        names: &[GlobalName],
        limits: Limits,
        budget: twine::CollectionBudget,
    ) -> Result<OperationForkAttempt, CallError> {
        let roots = names
            .iter()
            .map(|name| {
                self.runtime
                    .global_handle(name)
                    .map(twine::RetainedRoot::Thunk)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.collect_runtime_roots(&roots, limits, budget)
    }
    #[cfg(feature = "incremental")]
    fn collect_runtime_roots(
        &self,
        roots: &[twine::RetainedRoot],
        limits: Limits,
        budget: twine::CollectionBudget,
    ) -> Result<OperationForkAttempt, CallError> {
        let attempt = self.runtime.collect_snapshot(roots, limits, budget)?;
        Ok(OperationForkAttempt {
            work: attempt.work,
            session: attempt.fork.map(|fork| Self {
                runtime: fork.session,
                exports: self.exports.clone(),
                inheritance: Some(fork.inheritance),
                source_context: self.source_context.clone(),
            }),
        })
    }
    #[cfg(feature = "incremental")]
    pub fn try_fork(&self, limits: Limits) -> Result<OperationForkAttempt, CallError> {
        let attempt = self.runtime.try_fork(limits)?;
        Ok(OperationForkAttempt {
            work: attempt.work,
            session: attempt.fork.map(|fork| Self {
                runtime: fork.session,
                exports: self.exports.clone(),
                inheritance: Some(fork.inheritance),
                source_context: self.source_context.clone(),
            }),
        })
    }
    /// Inherit only a checked value that existed in this fork's direct baseline.
    /// The snapshot proof preserves its type, dictionary, capture and source scope.
    #[cfg(feature = "incremental")]
    pub fn inherit(&self, value: &TypedValue) -> Result<TypedValue, CallError> {
        let proof = self
            .inheritance
            .as_ref()
            .ok_or(CallError::Runtime(Outcome::ForeignValue))?;
        Ok(TypedValue {
            handle: self.runtime.inherit_thunk(proof, value.handle)?,
            ty: value.ty.clone(),
            origin: value.origin,
        })
    }

    pub(crate) fn observed_data_key(
        &self,
        value: &TypedValue,
    ) -> Result<Option<twine::ObservedDataKey>, CallError> {
        Ok(self.runtime.observed_data_key(value.handle, 64, 4096)?)
    }
    /// Copy compatible pure graphs into a fresh session using Twine's shared
    /// code/capture/dictionary checks. Old typed handles remain foreign; obtain
    /// new handles from this session's checked bindings. Reuse skips execution
    /// charges and accounts for copied heap/native storage normally.
    pub fn reuse_from(&mut self, previous: &OperationSession) -> twine::ReuseStats {
        self.runtime.reuse_from(&previous.runtime)
    }
    pub fn reuse_with_limits(
        &mut self,
        previous: &OperationSession,
        limits: twine::ReuseLimits,
    ) -> twine::ReuseStats {
        self.runtime.reuse_with_limits(&previous.runtime, limits)
    }

    /// Install a prepared direct extension atomically. It cannot replace code,
    /// reset budgets or import values from another session/revision.
    pub fn install(&mut self, prepared: &PreparedOperations) -> Result<(), CallError> {
        let extension = prepared
            .extension
            .as_ref()
            .ok_or(CallError::Extension(twine::ExtensionError::WrongBase))?;
        self.runtime
            .extend_program(extension)
            .map_err(CallError::Extension)?;
        self.exports = prepared.exports.clone();
        #[cfg(feature = "incremental")]
        {
            self.source_context = prepared.source_context.clone();
        }
        Ok(())
    }
    /// Obtain a lazy checked binding without forcing it.
    pub fn binding(&self, name: &str) -> Result<TypedValue, CallError> {
        let export = self
            .exports
            .get(name)
            .ok_or_else(|| CallError::UnknownBinding(name.into()))?;
        Ok(TypedValue {
            handle: self.runtime.global_handle(&export.name)?,
            ty: export.ty.clone(),
            origin: export.origin,
        })
    }
    /// Check every argument and its ownership before building a lazy application.
    /// Returned closures and partially applied functions retain their true type
    /// and runtime captures and can be supplied to subsequent calls in this session.
    pub fn apply(
        &mut self,
        function: &TypedValue,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, CallError> {
        self.runtime.inspection_state(function.handle)?;
        for argument in arguments {
            self.runtime.inspection_state(argument.handle)?;
        }
        let mut result = &function.ty;
        for (index, argument) in arguments.iter().enumerate() {
            let ValueType::Function(expected, output) = result else {
                return Err(CallError::NotAFunction);
            };
            if expected.as_ref() != &argument.ty {
                return Err(CallError::ArgumentType {
                    index,
                    expected: expected.clone(),
                    actual: Box::new(argument.ty.clone()),
                });
            }
            result = output;
        }
        let handle = self.runtime.apply(
            function.handle,
            &arguments.iter().map(|a| a.handle).collect::<Vec<_>>(),
        )?;
        Ok(TypedValue {
            handle,
            ty: result.clone(),
            origin: function.origin,
        })
    }
    /// Demand WHNF without running an IO action or decoding the result's fields.
    pub fn force(&mut self, value: &TypedValue) -> Result<ValueHandle, CallError> {
        Ok(self.runtime.force(value.handle)?)
    }
    /// Decode/observe existing values under this session's execution limits.
    /// Raw observations cannot be retagged as TypedValue or replace checked code.
    pub fn read_values(&mut self) -> twine::ValueReader<'_> {
        self.runtime.read_values()
    }
    pub fn view(&self, value: ValueHandle) -> Result<ValueView<'_>, CallError> {
        Ok(self.runtime.view(value)?)
    }
    /// Select a tuple/closed-record child using its checked structural type.
    /// Only the parent shell is forced; the child may be a closure or lazy value.
    /// Nominal destructuring uses an ordinary checked selector operation instead.
    pub fn project(&mut self, value: &TypedValue, field: &Field) -> Result<TypedValue, CallError> {
        self.runtime.inspection_state(value.handle)?;
        let (ty, offset) = match (&value.ty, field) {
            (ValueType::Tuple(types), Field::Index(i)) => (types.get(*i).cloned(), *i),
            (ValueType::Record(types), Field::Named(name)) => (
                types.get(name).cloned(),
                types.keys().position(|key| key == name).unwrap_or(0),
            ),
            _ => return Err(CallError::InvalidField(field.clone())),
        };
        let ty = ty.ok_or_else(|| CallError::InvalidField(field.clone()))?;
        let shell = self.runtime.force(value.handle)?;
        let shape_matches = match (&value.ty, self.runtime.view(shell)?) {
            (ValueType::Tuple(types), ValueView::Tuple(n)) => types.len() == n,
            (ValueType::Record(types), ValueView::Record(names)) => {
                types.keys().map(String::as_str).eq(names)
            }
            _ => false,
        };
        if !shape_matches {
            return Err(CallError::ValueShape);
        }
        let child = self
            .runtime
            .children(shell, offset, 1)?
            .into_iter()
            .next()
            .ok_or(CallError::ValueShape)?;
        if &child.field != field {
            return Err(CallError::ValueShape);
        }
        Ok(TypedValue {
            handle: child.handle,
            ty,
            origin: value.origin,
        })
    }
    pub(crate) fn restrict_execution(&mut self, steps: u64, heap_entries: usize) {
        self.runtime.restrict_execution(steps, heap_entries);
    }
    pub fn stats(&self) -> Stats {
        self.runtime.stats()
    }
    /// Bounded production dependency metadata; reading it does not force values.
    #[cfg(feature = "incremental")]
    pub fn evidence_stats(&self) -> twine::EvidenceStats {
        self.runtime.evidence_stats()
    }
    pub fn heap_entries(&self) -> usize {
        self.runtime.heap_entries()
    }
    pub(crate) fn runtime_mut(&mut self) -> &mut Session {
        &mut self.runtime
    }
}
