//! Install checked code without changing existing expressions, bindings or heaps.
use super::*;
use knot_core::link::ProgramExtension;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionError {
    WrongBase,
    Host(HostLinkError),
    Runtime(Outcome),
}
impl Session {
    /// Append checked operations to this exact code revision. No value is forced.
    /// Every fallible check precedes mutation; old handles and budgets remain valid.
    pub fn extend_program(&mut self, extension: &ProgramExtension) -> Result<(), ExtensionError> {
        self.cache_work(|session| session.extend_program_impl(extension))
    }
    fn extend_program_impl(&mut self, extension: &ProgramExtension) -> Result<(), ExtensionError> {
        if self.abandoned {
            return Err(ExtensionError::Runtime(Outcome::Abandoned));
        }
        if self.limits.cancellation.load(Ordering::Relaxed) {
            return Err(ExtensionError::Runtime(Outcome::Cancelled));
        }
        if !extension.accepts(&self.program) {
            #[cfg(feature = "incremental")]
            if self.source_code.is_some() {
                let staged = self.source_extension(extension)?;
                self.install_extension(staged.program, &staged.added)?;
                self.install_source_view(staged.view);
                return Ok(());
            }
            return Err(ExtensionError::WrongBase);
        }
        self.install_extension(extension.program(), extension.added_globals())
    }
    fn install_extension(
        &mut self,
        program: Arc<Program>,
        added: &[(Global, ExprId)],
    ) -> Result<(), ExtensionError> {
        if added.len() > self.limits.heap_entries.saturating_sub(self.heap_entries())
            || self.heap_entries() > self.limits.heap_entries
            || self.stats.steps >= self.limits.steps
        {
            return Err(ExtensionError::Runtime(Outcome::BudgetExhausted));
        }
        #[cfg(feature = "incremental")]
        {
            let copying = self.thunks.append_work(added.len()).saturating_add(
                self.global_refs.append_work(
                    program
                        .expressions
                        .len()
                        .saturating_sub(self.program.expressions.len()),
                ),
            );
            if copying > self.limits.steps.saturating_sub(self.stats.steps) {
                return Err(ExtensionError::Runtime(Outcome::BudgetExhausted));
            }
        }
        let prefix = self.program.expressions.len();
        for expr in program.expressions.iter_from(prefix) {
            if let Kind::Host(name, signature) = &expr.kind {
                let f = self.hosts.function(name).ok_or_else(|| {
                    ExtensionError::Host(HostLinkError::MissingFunction(name.clone()))
                })?;
                if f.signature != *signature {
                    return Err(ExtensionError::Host(HostLinkError::SignatureMismatch(
                        name.clone(),
                    )));
                }
            }
        }
        self.program = program;
        #[cfg(feature = "compact-captures")]
        self.program.expressions.capture_layouts();
        for (name, expr) in added {
            let thunk = self.delay_expression(*expr, 0);
            self.globals.insert(name.clone(), thunk);
        }
        self.global_refs
            .extend(
                self.program
                    .expressions
                    .iter_from(prefix)
                    .map(|expr| match &expr.kind {
                        Kind::Global(name) => self.globals.get(name).copied(),
                        _ => None,
                    }),
            );
        self.work_clock();
        Ok(())
    }
}
