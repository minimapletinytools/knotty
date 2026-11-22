//! Constraint generation: walks a `CExpr`/`CPattern` tree and produces a
//! `Constraint` tree plus a type for each node, without calling `unify` —
//! solving (`solve.rs`) is a separate pass.
//!
//! **Name references.** A reference to anything but a local binding
//! (`Ref::TopLevel`, `Ref::Imported`, `Ref::Builtin`) becomes a `Lookup`:
//! its scheme may not be known until an earlier binding group is solved,
//! so solving instantiates it. A lambda/case/do parameter resolves
//! immediately against `LocalScope`, with no constraint. A `let`-bound
//! name is also `Ref::Local`, but is generalized, so once its group is
//! done every reference to it is a `LookupLocal`, keyed by the binding's
//! own header `TypeVarId`.

pub mod decl;
pub mod expr;
pub mod pattern;

use std::collections::HashMap;

use knot_canonical::ast::Ref;
use knot_syntax::span::Span;

use crate::var::TypeVarId;

#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    True,
    Equal {
        span: Span,
        expected: TypeVarId,
        actual: TypeVarId,
    },
    /// `ty` must have `interface` (an integer literal's `Num`, say).
    HasInstance {
        span: Span,
        interface: String,
        ty: TypeVarId,
    },
    /// Deferred name lookup — see module docs.
    Lookup {
        span: Span,
        reference: Ref,
        expected: TypeVarId,
    },
    /// The `let`-bound counterpart of `Lookup`, keyed by the binding's own
    /// header `TypeVarId` (a `let`-bound name isn't unique, its arena index
    /// is).
    LookupLocal {
        span: Span,
        key: TypeVarId,
        expected: TypeVarId,
    },
    And(Vec<Constraint>),
    /// Schemes known before solving starts — every signed top-level
    /// binding's declared signature, so references to it (including from
    /// before its own group, and its own recursive calls) instantiate the
    /// signature instead of sharing one monomorphic type.
    Install(Vec<(crate::solve::SchemeKey, crate::ty::Scheme)>),
    /// A typed hole: always an error, reported with its solved type.
    Hole {
        span: Span,
        ty: TypeVarId,
    },
    /// One binding group (an SCC of a module's or a `let`'s bindings).
    /// `header_con` constrains the members' definitions and is solved at
    /// `level + 1`; then the group is closed (generalized, see `solve.rs`)
    /// and `body_con` — everything the group's names are visible to — is
    /// solved back at `level`.
    ///
    /// `top_level` picks where schemes go: the global `SchemeEnv` by name,
    /// or `solve.rs`'s `local_env` by header type (for `LookupLocal`).
    /// `generalize` is false for a `let` group containing a pattern binding
    /// (`(a, b) = ...`): those are monomorphic, and so is everything in a
    /// group with them. `pattern_tys` are those pattern bindings' types.
    Let {
        level: u32,
        top_level: bool,
        generalize: bool,
        members: Vec<LetMember>,
        pattern_tys: Vec<TypeVarId>,
        header_con: Box<Constraint>,
        body_con: Box<Constraint>,
    },
    /// An instance's methods, checked at `level + 1` with the instance's
    /// context (and any method-specific constraints) as `given` facts.
    Given {
        level: u32,
        given: Vec<(TypeVarId, String)>,
        body: Box<Constraint>,
    },
}

impl Constraint {
    /// Imported value schemes needed by this entire checked body, including
    /// untaken branches, patterns, nested bindings and instance methods. Types
    /// and instance dictionaries are imported separately and are never filtered.
    pub(crate) fn imported_lookups(
        &self,
    ) -> HashMap<Vec<String>, std::collections::HashSet<String>> {
        let mut names: HashMap<Vec<String>, std::collections::HashSet<String>> = HashMap::new();
        let mut pending = vec![self];
        while let Some(constraint) = pending.pop() {
            match constraint {
                Self::Lookup {
                    reference: Ref::Imported { module, name },
                    ..
                } => {
                    names
                        .entry(module.clone())
                        .or_default()
                        .insert(name.clone());
                }
                Self::And(parts) => pending.extend(parts),
                Self::Let {
                    header_con,
                    body_con,
                    ..
                } => {
                    pending.push(header_con);
                    pending.push(body_con);
                }
                Self::Given { body, .. } => pending.push(body),
                Self::True
                | Self::Equal { .. }
                | Self::HasInstance { .. }
                | Self::Lookup { .. }
                | Self::LookupLocal { .. }
                | Self::Install(_)
                | Self::Hole { .. } => {}
            }
        }
        names
    }
}

/// One binding within a `Constraint::Let` group.
#[derive(Debug, Clone, PartialEq)]
pub struct LetMember {
    pub name: String,
    pub span: Span,
    /// The placeholder type `header_con` was generated against — self/
    /// mutual references inside the group point straight at this.
    pub header_ty: TypeVarId,
    /// `Some` for a binding with an explicit signature — see
    /// `DeclaredScheme`. `None` means its `Scheme` needs the general
    /// `ftv`-difference `generalize` algorithm instead (plan §4/§5).
    pub declared: Option<DeclaredScheme>,
    /// This member's own value, fully elaborated — its
    /// params (if any) folded into one `TExpr::Lambda` wrapping the typed
    /// body, or just the typed body directly for a zero-param binding. This
    /// is what a whole-module or `let`-expression's elaborated form is
    /// ultimately built out of (`constrain::decl`'s own doc comment on how
    /// `constrain_group_chain` threads it through).
    pub elaborated_body: crate::ast::TypedExpr,
}

/// A signature restates a binding's scheme directly rather than leaving
/// anything for `generalize` to infer: `rigid_vars` are the signature's own
/// type variables (`Substitution::fresh_rigid` slots, plan §5), and `given`
/// are its `Ord a =>`-style obligations, expressed as `(rigid var,
/// interface)` pairs. These are "given," not "wanted": the body may freely
/// rely on them (checked against this list, not the real instance table —
/// there's no concrete type to look up for a rigid variable), and they
/// become the resulting `Scheme.constraints` for callers to discharge fresh
/// at each instantiation instead.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredScheme {
    pub rigid_vars: Vec<TypeVarId>,
    pub given: Vec<(TypeVarId, String)>,
}

/// What a name found in `LocalScope` actually means for constraint
/// generation — see `Constraint::LookupLocal`'s own doc comment for the
/// full story of why this distinction exists at all (in short:
/// `knot-canonical`'s `Ref::Local` can't tell a lambda parameter from a
/// `let`-bound name, but they need different treatment here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalBinding {
    /// A lambda/case/do param, or a group member's own name while that
    /// group's `header_con` is still being generated (self/mutual
    /// reference) — always resolves to this exact `TypeVarId`, every time,
    /// no constraint needed at all.
    Monomorphic(TypeVarId),
    /// A `let`-bound name, once its own group is done being defined —
    /// keyed by its own header `TypeVarId` rather than by name (a bare
    /// name isn't unique across different `let` blocks; the arena index
    /// always is). Each reference gets its own `Constraint::LookupLocal`,
    /// resolved to a fresh instantiation later — real let-polymorphism,
    /// the same mechanism `Ref::TopLevel`'s `Lookup` already gets.
    Generalizable(TypeVarId),
}

impl LocalBinding {
    /// The `TypeVarId` this binding is *about*, regardless of which case —
    /// used where the distinction doesn't matter yet (binding it in the
    /// first place, or a self/mutual reference during a group's own
    /// `header_con`, which is always monomorphic no matter what the name
    /// will be promoted to afterward).
    pub fn header_ty(self) -> TypeVarId {
        match self {
            LocalBinding::Monomorphic(ty) | LocalBinding::Generalizable(ty) => ty,
        }
    }
}

/// Tracks simple local bindings (lambda params, case/as patterns, do binds,
/// and `let`-bound names before *and* after they're promoted) during
/// constraint generation. Mirrors `knot_canonical::env::Env`'s scope-stack
/// shape, mapping to a `LocalBinding` instead of just tracking presence.
#[derive(Debug, Default)]
pub struct LocalScope {
    frames: Vec<HashMap<String, LocalBinding>>,
}

impl LocalScope {
    pub fn new() -> Self {
        LocalScope { frames: Vec::new() }
    }

    pub fn push(&mut self) {
        self.frames.push(HashMap::new());
    }

    pub fn pop(&mut self) {
        self.frames.pop();
    }

    /// Binds `name` monomorphically — what every lambda/case/do param gets,
    /// and what a `let`/top-level group's own members get *while* that
    /// group's `header_con` is being generated (see
    /// `promote_to_generalizable` for the `let`-specific next step).
    pub fn bind(&mut self, name: &str, ty: TypeVarId) {
        self.frames
            .last_mut()
            .expect("LocalScope::bind called with no open scope")
            .insert(name.to_string(), LocalBinding::Monomorphic(ty));
    }

    /// Switches an already-bound name, in the current (innermost) frame, to
    /// `Generalizable` — called by `constrain::decl` right after a
    /// `let`-expression group's own `header_con` has been generated, so
    /// every reference from that point on (a later sibling, or the `in`
    /// body) defers through `Constraint::LookupLocal` instead of resolving
    /// immediately. Never called for a top-level group — that frame is
    /// popped entirely at the same point instead, since top-level names are
    /// referenced via `Ref::TopLevel`/`Lookup`, not `LocalScope`, from then
    /// on.
    pub fn promote_to_generalizable(&mut self, name: &str) {
        let Some(ty) = self.try_lookup(name).map(LocalBinding::header_ty) else {
            return;
        };
        self.frames
            .last_mut()
            .expect("promote_to_generalizable called with no open scope")
            .insert(name.to_string(), LocalBinding::Generalizable(ty));
    }

    /// Test helper: the binding for a name that must be in scope.
    #[cfg(test)]
    pub fn lookup(&self, name: &str) -> LocalBinding {
        self.try_lookup(name)
            .unwrap_or_else(|| panic!("{name:?} not in scope"))
    }

    /// Non-panicking lookup. Also used for `Ref::TopLevel` names (see
    /// `constrain::expr::constrain_name_ref`) — while a `let`/top-level
    /// binding group is being checked (`constrain::decl`), its own members
    /// are pushed here just like any local binding, so a self/mutually-
    /// recursive reference resolves monomorphically instead of going
    /// through the deferred `Lookup` constraint. A `Ref::TopLevel` name
    /// belonging to some *other*, already-processed group correctly falls
    /// through (`None`) to that deferred path instead.
    pub fn try_lookup(&self, name: &str) -> Option<LocalBinding> {
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(name))
            .copied()
    }
}
