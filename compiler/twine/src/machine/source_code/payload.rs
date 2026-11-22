//! Price owned payload before equality, cloning or allocating traversal scratch.
//! Patterns use a bounded depth walk; wide inputs never build a wide worklist.
use super::*;
use prepare::charge;
fn name(n: &GlobalName, work: &mut u64, limits: &Limits) -> Option<()> {
    charge(work, n.name.len().checked_add(n.module.len())?, limits)?;
    for part in &n.module {
        charge(work, part.len(), limits)?;
    }
    Some(())
}
pub(super) fn global(g: &Global, work: &mut u64, limits: &Limits) -> Option<()> {
    charge(work, 1, limits)?;
    match g {
        Global::Value(n) => name(n, work, limits)?,
        Global::Instance(class, n) => {
            charge(work, class.len(), limits)?;
            name(n, work, limits)?;
        }
        Global::Default(class, method) => {
            charge(work, class.len().checked_add(method.len())?, limits)?;
        }
        Global::Structural(class, shape) => {
            charge(work, class.len(), limits)?;
            if let knot_core::StructuralShape::Record(fields) = shape {
                charge(work, fields.len(), limits)?;
                for field in fields {
                    charge(work, field.len(), limits)?;
                }
            }
        }
    }
    Some(())
}
fn field(f: &Field, work: &mut u64, limits: &Limits) -> Option<()> {
    charge(work, 1, limits)?;
    match f {
        Field::Named(s) | Field::Super(s) => charge(work, s.len(), limits)?,
        Field::Method(a, b) => charge(work, a.len().checked_add(b.len())?, limits)?,
        Field::Index(_) => {}
    }
    Some(())
}
fn pattern(p: &Pattern, depth: usize, work: &mut u64, limits: &Limits) -> Option<()> {
    if depth >= 128 {
        return None;
    }
    charge(work, 1, limits)?;
    match p {
        Pattern::As(_, p) => pattern(p, depth + 1, work, limits)?,
        Pattern::Ctor(n, ps) => {
            name(n, work, limits)?;
            for p in ps {
                pattern(p, depth + 1, work, limits)?;
            }
        }
        Pattern::Tuple(ps) => {
            for p in ps {
                pattern(p, depth + 1, work, limits)?;
            }
        }
        Pattern::Record(fields) => {
            for (n, _) in fields {
                charge(work, n.len().checked_add(1)?, limits)?;
            }
        }
        Pattern::Lit(lit) => charge(work, literal_bytes(lit), limits)?,
        _ => {}
    }
    Some(())
}
pub(super) fn kind(k: &Kind, work: &mut u64, limits: &Limits) -> Option<()> {
    charge(work, 1, limits)?;
    match k {
        Kind::Lit(lit) => charge(work, literal_bytes(lit), limits)?,
        Kind::Rational(d) => charge(
            work,
            (d.significand().bits().div_ceil(8) as usize)
                .checked_add(d.exponent().bits().div_ceil(8) as usize)?,
            limits,
        )?,
        Kind::Global(g) => global(g, work, limits)?,
        Kind::Ctor(n, _) => name(n, work, limits)?,
        Kind::Host(n, signature) => {
            name(n, work, limits)?;
            for ty in signature.parameters.iter().chain([&signature.result]) {
                charge(work, 1, limits)?;
                if let knot_core::HostType::Native(n) = ty {
                    name(n, work, limits)?;
                }
            }
        }
        Kind::Prim(s) | Kind::Unsupported(s) => charge(work, s.len(), limits)?,
        Kind::App(_, xs) | Kind::Tuple(xs) => charge(work, xs.len(), limits)?,
        Kind::Let(xs, _) => charge(work, xs.len().checked_mul(2)?, limits)?,
        Kind::Case(_, arms) => {
            for (p, _) in arms {
                pattern(p, 0, work, limits)?;
            }
        }
        Kind::Record(fields) | Kind::RecordUpdate(_, fields) => {
            for (n, _) in fields {
                charge(work, n.len().checked_add(1)?, limits)?;
            }
        }
        Kind::Select(_, f) => field(f, work, limits)?,
        Kind::Dict(fields) => {
            for (f, _) in fields {
                field(f, work, limits)?;
            }
        }
        Kind::Local(_) | Kind::Lam(..) => {}
    }
    Some(())
}
