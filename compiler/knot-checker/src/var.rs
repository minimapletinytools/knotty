//! Type variables as arena indices (`TypeVarId`) into a `Substitution`'s
//! copy-on-write pages of slots — union-find with path compression.
//!
//! A `Slot` is one of four things: `Unbound` (a flexible metavariable),
//! `Link` (a union-find redirect), `Bound` (resolved to a `Structure`), or
//! `Rigid` (a signature's own type variable — opaque, see `unify.rs`).
//!
//! **Levels.** Every `Unbound` and `Rigid` slot records the `let`-nesting
//! depth it belongs to. `Substitution::level` is the depth new variables
//! are created at; constraint generation and solving both raise it while
//! working inside a binding group's own definitions and lower it again
//! afterwards. Unification keeps levels honest — linking two variables
//! keeps the smaller level, and binding a variable to a structure lowers
//! every variable inside that structure to at most the bound variable's
//! level — so a variable's level is always the outermost scope it is
//! reachable from. Generalizing a group at level `L` then means exactly
//! "quantify the free variables whose level is still greater than `L`",
//! with no dependence on the order constraints happened to be solved in.

use crate::ty::Structure;
mod slots;
use slots::Slots;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeVarId(u32);

#[derive(Debug, Clone)]
enum Slot {
    Unbound { level: u32 },
    Link(TypeVarId),
    Bound(Structure),
    Rigid { name: String, level: u32 },
}

#[derive(Debug, Default, Clone)]
pub struct Substitution {
    slots: Slots,
    level: u32,
    next_node: u32,
}

impl Substitution {
    pub fn new() -> Self {
        Substitution {
            slots: Slots::default(),
            level: 0,
            next_node: 0,
        }
    }

    pub(crate) fn fresh_node(&mut self) -> crate::ast::NodeId {
        let id = crate::ast::NodeId(self.next_node);
        self.next_node = self.next_node.checked_add(1).expect("too many typed nodes");
        id
    }

    /// The level new variables are created at.
    pub fn level(&self) -> u32 {
        self.level
    }

    pub fn set_level(&mut self, level: u32) {
        self.level = level;
    }

    pub fn enter_level(&mut self) {
        self.level += 1;
    }

    pub fn exit_level(&mut self) {
        self.level -= 1;
    }

    pub fn fresh_unbound(&mut self) -> TypeVarId {
        let level = self.level;
        self.fresh_unbound_at(level)
    }

    pub fn fresh_unbound_at(&mut self, level: u32) -> TypeVarId {
        let id = TypeVarId(self.slots.len() as u32);
        self.slots.push(Slot::Unbound { level });
        id
    }

    /// A signature's own type variable, opaque to unification against any
    /// concrete type or any *other* rigid variable — see `unify.rs`.
    pub fn fresh_rigid(&mut self, name: String) -> TypeVarId {
        let id = TypeVarId(self.slots.len() as u32);
        self.slots.push(Slot::Rigid {
            name,
            level: self.level,
        });
        id
    }

    pub fn fresh_bound(&mut self, structure: Structure) -> TypeVarId {
        let id = TypeVarId(self.slots.len() as u32);
        self.slots.push(Slot::Bound(structure));
        id
    }

    /// Path-compressed find: follows `Link`s to the representative slot.
    pub fn find(&mut self, id: TypeVarId) -> TypeVarId {
        match self.slots[id.0 as usize] {
            Slot::Link(next) => {
                let root = self.find(next);
                if root != next {
                    self.slots[id.0 as usize] = Slot::Link(root);
                }
                root
            }
            _ => id,
        }
    }

    /// Inspect a frozen arena without cloning it just to compress lookup paths.
    /// Returns the representative identity and borrows its structure, if bound.
    pub(crate) fn read_root(&self, mut id: TypeVarId) -> (TypeVarId, Option<&Structure>) {
        loop {
            match &self.slots[id.0 as usize] {
                Slot::Link(next) => id = *next,
                Slot::Bound(shape) => return (id, Some(shape)),
                _ => return (id, None),
            }
        }
    }

    /// The structure at `id`'s root — `None` for an unbound or rigid variable.
    pub fn resolve_structure(&mut self, id: TypeVarId) -> Option<Structure> {
        let root = self.find(id);
        match &self.slots[root.0 as usize] {
            Slot::Bound(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Like `resolve_structure`, but a `VarApp` whose head has been pinned to
    /// a concrete constructor is rewritten into the equivalent `App` — the
    /// form every consumer that inspects a type's head wants.
    pub fn head_normal(&mut self, id: TypeVarId) -> Option<Structure> {
        match self.resolve_structure(id)? {
            Structure::VarApp(f, args) => match self.resolve_structure(f) {
                Some(Structure::Ctor(r, mut leading) | Structure::App(r, mut leading)) => {
                    leading.extend(args);
                    Some(Structure::App(r, leading))
                }
                _ => Some(Structure::VarApp(f, args)),
            },
            other => Some(other),
        }
    }

    /// The same head normalization for inspecting a frozen arena. Copies only
    /// the requested structure and does not compress paths or mutate slots.
    pub fn head_normal_readonly(&self, id: TypeVarId) -> Option<Structure> {
        let shape = self.read_root(id).1?;
        if let Structure::VarApp(head, args) = shape {
            if let Some(Structure::Ctor(name, leading) | Structure::App(name, leading)) =
                self.read_root(*head).1
            {
                return Some(Structure::App(
                    name.clone(),
                    leading.iter().chain(args).copied().collect(),
                ));
            }
        }
        Some(shape.clone())
    }

    pub fn is_rigid(&mut self, id: TypeVarId) -> bool {
        let root = self.find(id);
        matches!(self.slots[root.0 as usize], Slot::Rigid { .. })
    }

    pub fn is_unbound(&mut self, id: TypeVarId) -> bool {
        let root = self.find(id);
        matches!(self.slots[root.0 as usize], Slot::Unbound { .. })
    }

    pub fn rigid_name(&mut self, id: TypeVarId) -> Option<String> {
        let root = self.find(id);
        match &self.slots[root.0 as usize] {
            Slot::Rigid { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    /// The level of an unbound or rigid variable's root; `None` once bound.
    pub fn var_level(&mut self, id: TypeVarId) -> Option<u32> {
        let root = self.find(id);
        match self.slots[root.0 as usize] {
            Slot::Unbound { level } | Slot::Rigid { level, .. } => Some(level),
            _ => None,
        }
    }

    /// Lowers an unbound variable's level to at most `level`. No-op for
    /// anything else.
    pub fn lower_level(&mut self, id: TypeVarId, level: u32) {
        let root = self.find(id);
        if matches!(self.slots[root.0 as usize], Slot::Unbound { level: current } if current > level)
        {
            self.slots[root.0 as usize] = Slot::Unbound { level };
        }
    }

    /// Unions two *unbound* variables, keeping the smaller level.
    pub fn union_unbound(&mut self, a: TypeVarId, b: TypeVarId) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        let la = self.var_level(a).unwrap_or(0);
        let lb = self.var_level(b).unwrap_or(0);
        self.slots[b.0 as usize] = Slot::Link(a);
        self.slots[a.0 as usize] = Slot::Unbound { level: la.min(lb) };
    }

    /// Makes `from` an alias for `to` unconditionally (used to link a
    /// flexible variable to a rigid one, which must stay the root).
    pub fn link_to(&mut self, from: TypeVarId, to: TypeVarId) {
        let (from, to) = (self.find(from), self.find(to));
        if from != to {
            self.slots[from.0 as usize] = Slot::Link(to);
        }
    }

    /// Binds an unbound variable to a structure. The occurs check and level
    /// adjustment are the caller's responsibility (see `unify::bind`).
    pub fn bind(&mut self, id: TypeVarId, structure: Structure) {
        let root = self.find(id);
        self.slots[root.0 as usize] = Slot::Bound(structure);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_arenas_isolate_compression_binding_levels_and_appends_across_pages() {
        let mut original = Substitution::new();
        original.set_level(7);
        let vars = (0..600)
            .map(|_| original.fresh_unbound())
            .collect::<Vec<_>>();
        original.link_to(vars[1], vars[300]);
        original.link_to(vars[300], vars[599]);
        let mut left = original.clone();
        let mut right = original.clone();

        assert_eq!(left.find(vars[1]), vars[599]);
        left.bind(vars[599], Structure::Unit);
        right.lower_level(vars[1], 2);
        assert_eq!(left.resolve_structure(vars[1]), Some(Structure::Unit));
        assert!(right.is_unbound(vars[1]));
        assert_eq!(right.var_level(vars[599]), Some(2));
        assert_eq!(original.var_level(vars[1]), Some(7));

        // Each clone appends at the same index in its own partially full page.
        let rigid = original.fresh_rigid("a".into());
        let unit = left.fresh_bound(Structure::Unit);
        let flexible = right.fresh_unbound();
        assert_eq!(rigid, unit);
        assert_eq!(unit, flexible);
        original.link_to(vars[599], rigid);
        assert!(original.is_rigid(vars[1]));
        assert_eq!(left.resolve_structure(unit), Some(Structure::Unit));
        assert!(right.is_unbound(flexible));
        assert_eq!(left.resolve_structure(vars[1]), Some(Structure::Unit));
        assert_eq!(right.var_level(vars[1]), Some(2));
    }

    #[test]
    fn readonly_head_normalization_follows_links_and_preserves_the_arena() {
        use knot_canonical::ast::Ref;
        let mut sub = Substitution::new();
        let text = sub.fresh_bound(Structure::App(Ref::Builtin("String".into()), vec![]));
        let int = sub.fresh_bound(Structure::App(Ref::Builtin("Int".into()), vec![]));
        let head = sub.fresh_unbound();
        let alias = sub.fresh_unbound();
        sub.link_to(alias, head);
        let application = sub.fresh_bound(Structure::VarApp(alias, vec![int]));
        let rigid = sub.fresh_rigid("a".into());
        assert_eq!(sub.head_normal_readonly(head), None);
        assert_eq!(sub.head_normal_readonly(rigid), None);
        assert_eq!(
            sub.head_normal_readonly(application),
            Some(Structure::VarApp(alias, vec![int]))
        );
        for shape in [
            Structure::Ctor(Ref::Builtin("Result".into()), vec![text]),
            Structure::App(Ref::Builtin("Result".into()), vec![text]),
        ] {
            sub.bind(head, shape);
            let before = format!("{sub:?}");
            let expected = Some(Structure::App(
                Ref::Builtin("Result".into()),
                vec![text, int],
            ));
            assert_eq!(sub.head_normal_readonly(application), expected);
            assert_eq!(format!("{sub:?}"), before);
            assert_eq!(sub.clone().head_normal(application), expected);
        }
    }

    #[test]
    fn fresh_vars_are_distinct_and_unbound() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let b = sub.fresh_unbound();
        assert_ne!(a, b);
        assert_eq!(sub.find(a), a);
        assert!(sub.resolve_structure(a).is_none());
        assert!(!sub.is_rigid(a));
    }

    #[test]
    fn union_unbound_makes_find_agree() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let b = sub.fresh_unbound();
        sub.union_unbound(a, b);
        assert_eq!(sub.find(a), sub.find(b));
    }

    #[test]
    fn union_keeps_the_smaller_level() {
        let mut sub = Substitution::new();
        let outer = sub.fresh_unbound();
        sub.enter_level();
        let inner = sub.fresh_unbound();
        sub.union_unbound(inner, outer);
        assert_eq!(sub.var_level(inner), Some(0));
        assert_eq!(sub.var_level(outer), Some(0));
    }

    #[test]
    fn bind_resolves_to_the_given_structure() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        sub.bind(a, Structure::Unit);
        assert_eq!(sub.resolve_structure(a), Some(Structure::Unit));
    }

    #[test]
    fn rigid_var_reports_its_name_level_and_no_structure() {
        let mut sub = Substitution::new();
        sub.enter_level();
        let a = sub.fresh_rigid("a".to_string());
        assert!(sub.is_rigid(a));
        assert_eq!(sub.rigid_name(a), Some("a".to_string()));
        assert_eq!(sub.var_level(a), Some(1));
        assert!(sub.resolve_structure(a).is_none());
    }

    #[test]
    fn link_to_makes_the_flexible_side_disappear_into_the_target() {
        let mut sub = Substitution::new();
        let rigid = sub.fresh_rigid("a".to_string());
        let flexible = sub.fresh_unbound();
        sub.link_to(flexible, rigid);
        assert_eq!(sub.find(flexible), sub.find(rigid));
        assert!(sub.is_rigid(flexible));
    }
}
