//! Relocate a checked append through the exact receiving-prefix capability.
//! Stage only new view entries. Publish them after the atomic code installer,
//! copying the old view only when another snapshot still owns it.
use super::*;
use knot_core::link::ProgramExtension;
pub(in crate::machine) struct Appended {
    pub(in crate::machine) program: Arc<Program>,
    pub(in crate::machine) added: Vec<(Global, ExprId)>,
    pub(in crate::machine) view: ViewAppend,
}
pub(in crate::machine) struct ViewAppend {
    checked: Arc<Program>,
    expressions: std::ops::Range<usize>,
    locals: std::ops::Range<usize>,
    origins: HashMap<OriginKey, Origin>,
}
impl Session {
    pub(in crate::machine) fn install_source_view(&mut self, append: ViewAppend) {
        // All validation, cloning/growth allowance and cancellation checks have
        // completed. No fallible work follows the program installation.
        let view = Arc::make_mut(self.source_code.as_mut().expect("source view"));
        view.expressions
            .extend(append.expressions.clone().map(ExprId));
        view.active.extend(append.expressions.map(ExprId));
        view.locals.extend(append.locals.map(LocalId));
        view.origins.extend(append.origins);
        view.checked = append.checked;
    }
    pub(in crate::machine) fn source_extension(
        &mut self,
        extension: &ProgramExtension,
    ) -> Result<Appended, ExtensionError> {
        let view = self.source_code.as_ref().ok_or(ExtensionError::WrongBase)?;
        if !extension.accepts(&view.checked) {
            return Err(ExtensionError::WrongBase);
        }
        let target = extension.program();
        let old_expressions = view.checked.expressions.len();
        let old_locals = view.checked.next_local;
        let count = target
            .expressions
            .len()
            .checked_sub(old_expressions)
            .ok_or(ExtensionError::WrongBase)?;
        let locals = target
            .next_local
            .checked_sub(old_locals)
            .ok_or(ExtensionError::WrongBase)?;
        let mut work = self.stats.steps;
        let staged = (|| {
            let start = self.program.expressions.len();
            let end = start.checked_add(count)?;
            let local_start = self.program.next_local;
            let local_end = local_start.checked_add(locals)?;
            if end > view.retained_expressions
                || local_end > view.retained_expressions.saturating_mul(8)
            {
                return None;
            }
            let shared = Arc::strong_count(view) > 1;
            // A shared Vec clone may discard spare capacity. Charge that copy
            // and a possible subsequent growth; unique views pay only for growth.
            let growth = |len: usize, capacity: usize, added: usize| -> Option<usize> {
                let copying = if shared { len } else { 0 };
                let growing = added > capacity.saturating_sub(len) || (shared && added > 0);
                copying.checked_add(if growing { len } else { 0 })
            };
            let view_work = growth(view.active.len(), view.active.capacity(), count)?
                .checked_add(growth(view.origins.len(), view.origins.capacity(), count)?)?
                .checked_add(growth(
                    view.expressions.len(),
                    view.expressions.capacity(),
                    count,
                )?)?
                .checked_add(growth(view.locals.len(), view.locals.capacity(), locals)?)?
                .checked_add(count)?
                .checked_add(locals)?
                .checked_add(extension.added_globals().len())?
                .checked_mul(128)?;
            prepare::charge(&mut work, view_work, &self.limits)?;
            // Persistent Program tables are still staged. Bound their directory
            // copies and all potentially copied debug/global-name payload. The
            // final expression chunk is the only old executable payload copied.
            prepare::charge(&mut work, start.checked_add(local_start)?, &self.limits)?;
            for expr in self.program.expressions.append_copy_prefix() {
                payload::kind(&expr.kind, &mut work, &self.limits)?;
            }
            for global in self.program.globals.keys() {
                payload::global(global, &mut work, &self.limits)?;
            }
            for (_, name) in self.program.local_names.iter() {
                prepare::charge(&mut work, name.len().checked_add(1)?, &self.limits)?;
            }
            for expr in target.expressions.iter_from(old_expressions) {
                payload::kind(&expr.kind, &mut work, &self.limits)?;
            }
            for (global, _) in extension.added_globals() {
                payload::global(global, &mut work, &self.limits)?;
            }
            let expression = |id: &mut ExprId| -> Option<()> {
                *id = if id.0 < old_expressions {
                    *view.expressions.get(id.0)?
                } else if id.0 < target.expressions.len() {
                    ExprId(start.checked_add(id.0 - old_expressions)?)
                } else {
                    return None;
                };
                Some(())
            };
            let local = |id: &mut LocalId| -> Option<()> {
                *id = if id.0 < old_locals {
                    *view.locals.get(id.0)?
                } else if id.0 < target.next_local {
                    LocalId(local_start.checked_add(id.0 - old_locals)?)
                } else {
                    return None;
                };
                Some(())
            };
            let mut program = self.program.as_ref().clone();
            let mut origins = HashMap::new();
            for expr in target.expressions.iter_from(old_expressions) {
                prepare::charge(&mut work, 1, &self.limits)?;
                let mut expr = expr.clone();
                prepare::rewrite(&mut expr.kind, &expression, &local)?;
                let key = key(expr.origin);
                if view
                    .origins
                    .get(&key)
                    .is_some_and(|old| *old != expr.origin)
                    || origins
                        .insert(key, expr.origin)
                        .is_some_and(|old| old != expr.origin)
                {
                    return None;
                }
                program.expressions.push(expr);
            }
            let mut added = Vec::with_capacity(extension.added_globals().len());
            for (global, id) in extension.added_globals() {
                let mut id = *id;
                expression(&mut id)?;
                program.globals.insert(global.clone(), id);
                added.push((global.clone(), id));
            }
            for (id, name) in target
                .local_names
                .iter()
                .filter(|(id, _)| id.0 >= old_locals)
            {
                prepare::charge(&mut work, name.len().checked_add(1)?, &self.limits)?;
                let mut id = *id;
                local(&mut id)?;
                program.local_names.insert(id, name.clone());
            }
            program.next_local = local_end;
            Some(Appended {
                program: Arc::new(program),
                added,
                view: ViewAppend {
                    checked: target,
                    expressions: start..end,
                    locals: local_start..local_end,
                    origins,
                },
            })
        })();
        self.stats.steps = work;
        if self.limits.cancellation.load(Ordering::Relaxed) {
            return Err(ExtensionError::Runtime(Outcome::Cancelled));
        }
        staged.ok_or(ExtensionError::Runtime(Outcome::BudgetExhausted))
    }
}
