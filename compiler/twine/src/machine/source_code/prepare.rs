use super::*;
pub(super) struct Prepared {
    pub program: Program,
    pub view: View,
    pub reused: usize,
    pub appended: usize,
    pub identity_layout: bool,
}
pub(super) fn charge(work: &mut u64, amount: usize, limits: &Limits) -> Option<()> {
    if limits.cancellation.load(Ordering::Relaxed)
        || amount as u64 > limits.steps.saturating_sub(*work)
    {
        return None;
    }
    *work += amount as u64;
    Some(())
}
pub(super) fn prepare(
    old: &Program,
    target: &Arc<Program>,
    limits: &Limits,
    bound: SourceForkLimits,
    work: &mut u64,
) -> Option<Prepared> {
    if old.expressions.len() > bound.retained_expressions
        || target.expressions.len() > bound.retained_expressions
        || old.next_local > bound.retained_expressions.saturating_mul(8)
        || target.next_local > bound.retained_expressions.saturating_mul(8)
    {
        return None;
    }
    charge(
        work,
        old.expressions
            .len()
            .checked_add(old.next_local.checked_mul(32)?)?
            .checked_add(target.expressions.len().checked_mul(128)?)?
            .checked_add(target.next_local.checked_mul(32)?)?,
        limits,
    )?;
    // Bound cloning/comparison payload before constructing temporary shapes.
    // Nested source patterns have a separate conservative preparation depth cap.
    let mut comparison_unit = 1;
    for program in [old, target] {
        for expr in &program.expressions {
            let before = *work;
            super::payload::kind(&expr.kind, work, limits)?;
            comparison_unit = comparison_unit.max((*work - before) as usize);
        }
        for global in program.globals.keys() {
            super::payload::global(global, work, limits)?;
        }
    }
    let roots = target.globals.keys().cloned().collect::<Vec<_>>();
    let mut remaining = bound
        .code_nodes
        .min(limits.steps.saturating_sub(*work) as usize / comparison_unit);
    let before = remaining;
    let versions = knot_core::equivalence::source_versions(old, target, &roots, &mut remaining);
    *work += (before - remaining).checked_mul(comparison_unit)? as u64;
    let versions = versions?;
    let mut expressions = vec![None; target.expressions.len()];
    for (a, b) in versions.expressions() {
        *expressions.get_mut(b.0)? = Some(a);
    }
    let reused = expressions.iter().filter(|id| id.is_some()).count();
    let mut next = old.expressions.len();
    for id in &mut expressions {
        if id.is_none() {
            *id = Some(ExprId(next));
            next = next.checked_add(1)?;
        }
    }
    if next > bound.retained_expressions {
        return None;
    }
    let mut locals = vec![None; target.next_local];
    for (a, b) in versions.locals() {
        *locals.get_mut(b.0)? = Some(a);
    }
    let mut next_local = old.next_local;
    for id in &mut locals {
        if id.is_none() {
            *id = Some(LocalId(next_local));
            next_local = next_local.checked_add(1)?;
        }
    }
    if next_local > bound.retained_expressions.saturating_mul(8) {
        return None;
    }
    let expression = |id: &mut ExprId| -> Option<()> {
        *id = (*expressions.get(id.0)?)?;
        Some(())
    };
    let local = |id: &mut LocalId| -> Option<()> {
        *id = (*locals.get(id.0)?)?;
        Some(())
    };
    let mut program = old.clone();
    let mut view = View {
        active: Default::default(),
        origins: Default::default(),
        checked: target.clone(),
        expressions: expressions.iter().copied().collect::<Option<Vec<_>>>()?,
        locals: locals.iter().copied().collect::<Option<Vec<_>>>()?,
        retained_expressions: bound.retained_expressions,
    };
    for (index, expr) in target.expressions.iter().enumerate() {
        charge(work, 1, limits)?;
        let id = expressions[index]?;
        view.active.insert(id);
        let canonical = if id.0 < old.expressions.len() {
            old.expressions[id.0].origin
        } else {
            expr.origin
        };
        if view
            .origins
            .insert(key(canonical), expr.origin)
            .is_some_and(|other| other != expr.origin)
        {
            return None;
        }
        if id.0 < old.expressions.len() {
            continue;
        }
        let mut expr = expr.clone();
        rewrite(&mut expr.kind, &expression, &local)?;
        if id.0 != program.expressions.len() {
            return None;
        }
        program.expressions.push(expr);
    }
    // A changed binding must retire its previous executable root. Otherwise a
    // certificate for that root could remain active through another binding's
    // shared core node and falsely validate the changed binding. Ordinary Knot
    // lowering gives definitions distinct roots; ambiguous hand-shared core
    // conservatively uses the existing copied-transfer/fresh path.
    for (global, old_root) in old.globals.iter() {
        let current = target
            .globals
            .get(global)
            .and_then(|root| expressions[root.0]);
        if current != Some(*old_root) && view.active.contains(old_root) {
            return None;
        }
    }
    program.globals = Default::default();
    for (name, id) in target.globals.iter() {
        charge(work, 1, limits)?;
        program
            .globals
            .insert(name.clone(), expressions.get(id.0).copied()??);
    }
    for (id, name) in target.local_names.iter() {
        charge(work, name.len().saturating_add(1), limits)?;
        program
            .local_names
            .insert(locals.get(id.0).copied()??, name.clone());
    }
    program.next_local = next_local;
    // Whole-graph code proof plus exact arena slots/origins permits adopting
    // the receiving checked Arc without retaining a relocation view. This does
    // not certify input values or grant inheritance of old handles.
    let identity_layout = if reused == old.expressions.len()
        && target.expressions.len() == old.expressions.len()
        && next == old.expressions.len()
        && target.next_local == old.next_local
        && next_local == old.next_local
    {
        charge(
            work,
            next.checked_add(next_local)?
                .checked_add(target.globals.iter().count().checked_mul(128)?)?,
            limits,
        )?;
        expressions.iter().enumerate().all(|(i, id)| {
            *id == Some(ExprId(i)) && target.expressions[i].origin == old.expressions[i].origin
        }) && locals
            .iter()
            .enumerate()
            .all(|(i, id)| *id == Some(LocalId(i)))
            && target.globals.iter().count() == old.globals.iter().count()
            && target
                .globals
                .iter()
                .all(|(name, id)| old.globals.get(name) == Some(id))
    } else {
        false
    };
    Some(Prepared {
        identity_layout,
        appended: next - old.expressions.len(),
        program,
        view,
        reused,
    })
}
pub(super) fn rewrite(
    kind: &mut Kind,
    expression: &impl Fn(&mut ExprId) -> Option<()>,
    local: &impl Fn(&mut LocalId) -> Option<()>,
) -> Option<()> {
    match kind {
        Kind::Local(id) => local(id)?,
        Kind::Lam(id, body) => {
            local(id)?;
            expression(body)?;
        }
        Kind::Select(body, _) => expression(body)?,
        Kind::App(f, args) => {
            expression(f)?;
            for arg in args {
                expression(arg)?;
            }
        }
        Kind::Let(bindings, body) => {
            for (id, expr) in bindings {
                local(id)?;
                expression(expr)?;
            }
            expression(body)?;
        }
        Kind::Case(value, arms) => {
            expression(value)?;
            for (pattern, body) in arms {
                expression(body)?;
                let mut pending = vec![pattern];
                while let Some(pattern) = pending.pop() {
                    match pattern {
                        Pattern::Bind(id) => local(id)?,
                        Pattern::As(id, child) => {
                            local(id)?;
                            pending.push(child);
                        }
                        Pattern::Ctor(_, fields) | Pattern::Tuple(fields) => pending.extend(fields),
                        Pattern::Record(fields) => {
                            for (_, id) in fields {
                                local(id)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Kind::Tuple(fields) => {
            for field in fields {
                expression(field)?;
            }
        }
        Kind::Record(fields) => {
            for (_, field) in fields {
                expression(field)?;
            }
        }
        Kind::Dict(fields) => {
            for (_, field) in fields {
                expression(field)?;
            }
        }
        Kind::RecordUpdate(base, fields) => {
            expression(base)?;
            for (_, field) in fields {
                expression(field)?;
            }
        }
        _ => {}
    }
    Some(())
}
