//! Scope tracking and name resolution shared by every `resolve::*` module.
//!
//! Two modes, matching `knot_syntax`'s own `parse` vs `parse_decls` split:
//!
//! - **Module mode** (`Env::for_module`, used by `canonicalize_module`): the
//!   module's real `import` list is known, so a qualified reference's
//!   qualifier (`Foo` in `Foo.bar`) must match a real import's alias or module
//!   path — always checkable locally, no other module's contents needed.
//! - **Snippet mode** (`Env::for_decls`, used by `canonicalize_decls`, for the
//!   same bare-declaration-list fixtures `knot-syntax`'s own corpus harness
//!   parses via `parse_decls`): there is no import list at all, so a qualified
//!   reference is trusted at face value — otherwise every existing syntax-only
//!   corpus fixture that writes e.g. `Map.fromList` with no surrounding module
//!   header would spuriously fail name resolution. Unqualified names are still
//!   checked in both modes; only qualifier trust differs.
//!
//! The project loader in knot-workbench supplies checked interfaces through
//! the optional `ModuleRegistry`. It validates the entire import/export lists;
//! without it, cross-module questions this crate can't answer on its own
//! (does module X really export Y?) are resolved permissively rather than
//! rejected, documented at each such spot below.

use std::collections::{HashMap, HashSet};

use knot_syntax::ast::decl::{ExposedItem, Exposing, Fixity, Import};

use crate::ast::Ref;
use crate::prelude;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Value,
    Type,
    Ctor,
}

#[derive(Debug, Clone)]
pub struct CtorInfo {
    pub arity: usize,
    pub type_name: String,
}

/// What another module actually exports, when known. `constructors` maps a
/// *type* name to its variant names (for expanding a `Foo(..)` exposing item).
///
/// Exported operators are plain `values` (`"+++"`). Additional alias, fixity
/// and constructor metadata comes from the corresponding ModuleRegistry methods.
#[derive(Debug, Clone, Default)]
pub struct ModuleInterface {
    pub values: HashSet<String>,
    pub types: HashSet<String>,
    pub constructors: HashMap<String, Vec<String>>,
}

/// Export metadata is treated as a snapshot for one name-resolution pass.
pub trait ModuleRegistry {
    fn exports(&self, module: &[String]) -> Option<ModuleInterface>;
    /// Already expanded alias bodies, with nominal references qualified.
    fn aliases(&self, _module: &[String]) -> HashMap<String, (Vec<String>, crate::ast::CType)> {
        HashMap::new()
    }
    fn fixity(&self, _module: &[String], _name: &str) -> Option<Fixity> {
        None
    }
    fn constructor(&self, _module: &[String], _name: &str) -> Option<CtorInfo> {
        None
    }
}

pub enum ExposedLookup {
    Found(Vec<String>),
    Ambiguous(Vec<Vec<String>>),
    NotFound,
}

/// Tracks, for one namespace, which modules bring a given unqualified name
/// into scope. `wildcard_fallback` holds modules whose `exposing (..)` (module
/// header or import) couldn't be expanded into concrete names because no
/// `ModuleRegistry` was supplied — see module docs. It's a deliberately
/// last-resort, imprecise bucket: with no registry, an unresolved unqualified
/// name that could plausibly have come from *some* wildcard import is trusted
/// against the first such import instead of being rejected, matching the
/// same "permissive without real project knowledge" stance qualified
/// references already take.
#[derive(Default)]
struct ExposedTable {
    explicit: HashMap<String, Vec<Vec<String>>>,
    wildcard_fallback: Vec<Vec<String>>,
}

impl ExposedTable {
    fn add_explicit(&mut self, name: String, module: Vec<String>) {
        let modules = self.explicit.entry(name).or_default();
        if !modules.contains(&module) {
            modules.push(module);
        }
    }

    fn add_wildcard_fallback(&mut self, module: Vec<String>) {
        self.wildcard_fallback.push(module);
    }

    /// A name an import lists by name, which takes precedence over the
    /// prelude's (`import String exposing (length)` means `String.length`).
    fn resolve_explicit(&self, name: &str) -> Option<ExposedLookup> {
        self.explicit.get(name).map(|modules| {
            if modules.len() == 1 {
                ExposedLookup::Found(modules[0].clone())
            } else {
                ExposedLookup::Ambiguous(modules.clone())
            }
        })
    }

    fn resolve(&self, name: &str) -> ExposedLookup {
        if let Some(found) = self.resolve_explicit(name) {
            return found;
        }
        match self.wildcard_fallback.first() {
            Some(m) => ExposedLookup::Found(m.clone()),
            None => ExposedLookup::NotFound,
        }
    }
}

pub struct Env<'a> {
    /// Local (lambda/let/case/do-bind) scopes, innermost last. Only values are
    /// ever locally scoped -- types and constructors are always module- or
    /// import-level.
    scopes: Vec<HashSet<String>>,
    top_level_values: HashSet<String>,
    /// Fixity declarations for this module's own operators.
    top_level_fixities: HashMap<String, Fixity>,
    top_level_types: HashSet<String>,
    /// Prelude constructors plus every locally-declared ADT's variants,
    /// populated before any declaration body is resolved (see `resolve/decl.rs`).
    constructors: HashMap<String, CtorInfo>,
    own_constructors: HashSet<String>,
    /// Qualifier (as written, dot-joined -- an alias, or a full unaliased
    /// module path) -> the real module path it refers to. Empty and never
    /// consulted in snippet mode.
    import_qualifiers: HashMap<String, Vec<String>>,
    // Fetch each actual module at most once, when exposure or a qualified use
    // needs it. Aliases share the snapshot, including unknown interfaces.
    import_interfaces: HashMap<Vec<String>, std::cell::OnceCell<Option<ModuleInterface>>>,
    exposed_values: ExposedTable,
    exposed_types: ExposedTable,
    exposed_ctors: ExposedTable,
    strict_qualifiers: bool,
    registry: Option<&'a dyn ModuleRegistry>,
    /// Set only while resolving the Prelude itself: its own top-level names
    /// are `Ref::Builtin`, as they are from everywhere else.
    own_names_builtin: bool,
}

impl<'a> Env<'a> {
    fn empty(strict_qualifiers: bool, registry: Option<&'a dyn ModuleRegistry>) -> Self {
        let mut constructors = HashMap::new();
        for (name, arity, type_name) in prelude::builtin_constructors() {
            constructors.insert(
                name.to_string(),
                CtorInfo {
                    arity,
                    type_name: type_name.to_string(),
                },
            );
        }
        Env {
            scopes: Vec::new(),
            top_level_values: HashSet::new(),
            top_level_fixities: HashMap::new(),
            top_level_types: HashSet::new(),
            constructors,
            own_constructors: HashSet::new(),
            import_qualifiers: HashMap::new(),
            import_interfaces: HashMap::new(),
            exposed_values: ExposedTable::default(),
            exposed_types: ExposedTable::default(),
            exposed_ctors: ExposedTable::default(),
            strict_qualifiers,
            registry,
            own_names_builtin: false,
        }
    }

    /// A bare declaration list with no module header/imports (`parse_decls`'s
    /// world) -- qualified references are always trusted (see module docs).
    pub fn for_decls() -> Self {
        Env::empty(false, None)
    }

    /// A full module with a real import list -- qualifiers are checked against
    /// it. `registry`, if given, additionally lets `exposing (..)` expand to
    /// real names and lets qualified/exposed references be checked against
    /// what the target module actually exports.
    pub fn for_module(imports: &[Import], registry: Option<&'a dyn ModuleRegistry>) -> Self {
        let mut env = Env::empty(true, registry);
        let mut interfaces: HashMap<_, std::cell::OnceCell<Option<ModuleInterface>>> =
            HashMap::new();
        for import in imports {
            let slot = interfaces.entry(import.module.clone()).or_default();
            let known = import.exposing.as_ref().and_then(|_| {
                slot.get_or_init(|| registry.and_then(|r| r.exports(&import.module)))
                    .as_ref()
            });
            env.add_import(import, known);
        }
        env.import_interfaces = interfaces;
        env
    }

    fn add_import(&mut self, import: &Import, known: Option<&ModuleInterface>) {
        let qualifier = match &import.alias {
            Some(alias) => alias.clone(),
            None => import.module.join("."),
        };
        self.import_qualifiers
            .insert(qualifier, import.module.clone());

        match &import.exposing {
            None => {}
            Some(Exposing::All) => self.add_wildcard_exposing(&import.module, known),
            Some(Exposing::Some(items)) => {
                for item in items {
                    self.add_exposed_item(&import.module, item, known);
                }
            }
        }
    }

    fn add_wildcard_exposing(&mut self, module: &[String], known: Option<&ModuleInterface>) {
        match known {
            Some(iface) => {
                for name in &iface.values {
                    self.exposed_values
                        .add_explicit(name.clone(), module.to_vec());
                }
                for name in &iface.types {
                    self.exposed_types
                        .add_explicit(name.clone(), module.to_vec());
                }
                for names in iface.constructors.values() {
                    for name in names {
                        self.exposed_ctors
                            .add_explicit(name.clone(), module.to_vec());
                    }
                }
            }
            None => {
                self.exposed_values.add_wildcard_fallback(module.to_vec());
                self.exposed_types.add_wildcard_fallback(module.to_vec());
                self.exposed_ctors.add_wildcard_fallback(module.to_vec());
            }
        }
    }

    fn add_exposed_item(
        &mut self,
        module: &[String],
        item: &ExposedItem,
        known: Option<&ModuleInterface>,
    ) {
        match item {
            ExposedItem::Value(name) => {
                self.exposed_values
                    .add_explicit(name.clone(), module.to_vec());
            }
            ExposedItem::TypeOnly(name) => {
                self.exposed_types
                    .add_explicit(name.clone(), module.to_vec());
            }
            ExposedItem::TypeWithVariants(name) => {
                self.exposed_types
                    .add_explicit(name.clone(), module.to_vec());
                match known.and_then(|iface| iface.constructors.get(name)) {
                    Some(variants) => {
                        for variant in variants {
                            self.exposed_ctors
                                .add_explicit(variant.clone(), module.to_vec());
                        }
                    }
                    None => self.exposed_ctors.add_wildcard_fallback(module.to_vec()),
                }
            }
        }
    }

    // -- top-level declaration bookkeeping (populated before any body is resolved) --

    pub fn declare_top_level_value(&mut self, name: &str) {
        self.top_level_values.insert(name.to_string());
    }

    pub fn set_own_names_builtin(&mut self, on: bool) {
        self.own_names_builtin = on;
    }

    fn own(&self, name: &str) -> Ref {
        if self.own_names_builtin {
            Ref::Builtin(name.to_string())
        } else {
            Ref::TopLevel(name.to_string())
        }
    }

    pub fn declare_fixity(&mut self, name: &str, fixity: Fixity) {
        self.top_level_fixities.insert(name.to_string(), fixity);
    }

    /// How a resolved operator groups: its own module's declaration, or the
    /// prelude's for a builtin, an imported registry fixity, or `infixl 9`.
    /// Local binders cannot declare fixities; a registry may omit metadata.
    pub fn fixity_of(&self, op: &Ref) -> Fixity {
        let declared = match op {
            Ref::TopLevel(name) => self.top_level_fixities.get(name).copied(),
            Ref::Builtin(name) => prelude::builtin_fixity(name),
            Ref::Imported { module, name } => self.registry.and_then(|r| r.fixity(module, name)),
            _ => None,
        };
        declared.unwrap_or(Fixity::DEFAULT)
    }

    pub fn declare_top_level_type(&mut self, name: &str) {
        self.top_level_types.insert(name.to_string());
    }

    pub fn declare_ctor(&mut self, name: &str, arity: usize, type_name: &str) {
        self.own_constructors.insert(name.to_string());
        self.constructors.insert(
            name.to_string(),
            CtorInfo {
                arity,
                type_name: type_name.to_string(),
            },
        );
    }

    pub fn ctor_info(&self, name: &str) -> Option<&CtorInfo> {
        self.constructors.get(name)
    }

    // -- local scoping --

    pub fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Binds `name` in the innermost scope. Callers that need duplicate-in-one-
    /// pattern detection track their own set across a single pattern's sibling
    /// binders and don't rely on this for that -- shadowing across *different*
    /// lexical layers (e.g. a `let` shadowing an outer `let`) is always fine.
    pub fn bind_local(&mut self, name: &str) {
        self.scopes
            .last_mut()
            .expect("bind_local called with no open scope")
            .insert(name.to_string());
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|frame| frame.contains(name))
    }

    // -- resolution --

    /// Splits `name` on its *last* `.` -- everything before is the qualifier
    /// (itself possibly dotted, for a multi-segment module path), everything
    /// after is the unqualified name. `None` if `name` has no `.` at all.
    fn split_qualified(name: &str) -> Option<(&str, &str)> {
        name.rsplit_once('.')
    }

    pub fn resolve_value(&self, name: &str) -> Result<Ref, UnresolvedKind> {
        if let Some((qualifier, unqualified)) = Self::split_qualified(name) {
            return self.resolve_qualified(qualifier, unqualified, Namespace::Value);
        }
        if self.is_local(name) {
            return Ok(Ref::Local(name.to_string()));
        }
        if self.top_level_values.contains(name) {
            return Ok(self.own(name));
        }
        if let Some(found) = self.exposed_values.resolve_explicit(name) {
            return exposed_ref(found, name);
        }
        if prelude::is_builtin_value(name) {
            return Ok(Ref::Builtin(name.to_string()));
        }
        match self.exposed_values.resolve(name) {
            ExposedLookup::Found(module) => Ok(Ref::Imported {
                module,
                name: name.to_string(),
            }),
            ExposedLookup::Ambiguous(modules) => Err(UnresolvedKind::Ambiguous(modules)),
            ExposedLookup::NotFound => Err(UnresolvedKind::Unbound),
        }
    }

    /// Constructors used as *values* (e.g. `Just` applied like a function) and
    /// constructors used in pattern position share the same namespace/table.
    pub fn resolve_ctor(&self, name: &str) -> Result<(Ref, Option<CtorInfo>), UnresolvedKind> {
        if let Some((qualifier, unqualified)) = Self::split_qualified(name) {
            let r = self.resolve_qualified(qualifier, unqualified, Namespace::Ctor)?;
            let info = match &r {
                Ref::Imported { module, name } => {
                    self.registry.and_then(|r| r.constructor(module, name))
                }
                Ref::Builtin(name) => {
                    prelude::builtin_constructor(name).map(|(arity, type_name)| CtorInfo {
                        arity,
                        type_name: type_name.to_string(),
                    })
                }
                _ => None,
            };
            return Ok((r, info));
        }
        if self.own_constructors.contains(name) {
            return Ok((self.own(name), self.constructors.get(name).cloned()));
        }
        if let Some(found) = self.exposed_ctors.resolve_explicit(name) {
            let r = exposed_ref(found, name)?;
            let info = match &r {
                Ref::Imported { module, name } => {
                    self.registry.and_then(|r| r.constructor(module, name))
                }
                Ref::Builtin(name) => {
                    prelude::builtin_constructor(name).map(|(arity, type_name)| CtorInfo {
                        arity,
                        type_name: type_name.to_string(),
                    })
                }
                _ => None,
            };
            return Ok((r, info));
        }
        if let Some(info) = self.constructors.get(name) {
            return Ok((Ref::Builtin(name.to_string()), Some(info.clone())));
        }
        match self.exposed_ctors.resolve(name) {
            ExposedLookup::Found(module) => {
                let info = self.registry.and_then(|r| r.constructor(&module, name));
                Ok((
                    Ref::Imported {
                        module,
                        name: name.to_string(),
                    },
                    info,
                ))
            }
            ExposedLookup::Ambiguous(modules) => Err(UnresolvedKind::Ambiguous(modules)),
            ExposedLookup::NotFound => Err(UnresolvedKind::Unbound),
        }
    }

    pub fn resolve_type(&self, name: &str) -> Result<Ref, UnresolvedKind> {
        if let Some((qualifier, unqualified)) = Self::split_qualified(name) {
            return self.resolve_qualified(qualifier, unqualified, Namespace::Type);
        }
        if self.top_level_types.contains(name) {
            return Ok(self.own(name));
        }
        if let Some(found) = self.exposed_types.resolve_explicit(name) {
            return exposed_ref(found, name);
        }
        if prelude::is_builtin_type(name) {
            return Ok(Ref::Builtin(name.to_string()));
        }
        match self.exposed_types.resolve(name) {
            ExposedLookup::Found(module) => Ok(Ref::Imported {
                module,
                name: name.to_string(),
            }),
            ExposedLookup::Ambiguous(modules) => Err(UnresolvedKind::Ambiguous(modules)),
            ExposedLookup::NotFound => Err(UnresolvedKind::Unbound),
        }
    }

    fn resolve_qualified(
        &self,
        qualifier: &str,
        unqualified: &str,
        namespace: Namespace,
    ) -> Result<Ref, UnresolvedKind> {
        // As in Haskell, the implicitly imported Prelude can also be named
        // qualified (`Prelude.reverse`), which reaches a prelude name that a
        // top-level definition shadows.
        if qualifier == "Prelude" && !self.import_qualifiers.contains_key(qualifier) {
            let known = match namespace {
                Namespace::Value => prelude::is_builtin_value(unqualified),
                Namespace::Type => prelude::is_builtin_type(unqualified),
                Namespace::Ctor => prelude::builtin_constructor(unqualified).is_some(),
            };
            return if known {
                Ok(Ref::Builtin(unqualified.to_string()))
            } else {
                Err(UnresolvedKind::Unbound)
            };
        }
        let module = if self.strict_qualifiers {
            match self.import_qualifiers.get(qualifier) {
                Some(module) => module.clone(),
                None => return Err(UnresolvedKind::UnknownQualifier),
            }
        } else {
            qualifier.split('.').map(str::to_string).collect()
        };
        if let Some(iface) = self.import_interfaces.get(&module).and_then(|slot| {
            slot.get_or_init(|| self.registry.and_then(|r| r.exports(&module)))
                .as_ref()
        }) {
            let exported = match namespace {
                Namespace::Value => iface.values.contains(unqualified),
                Namespace::Type => iface.types.contains(unqualified),
                Namespace::Ctor => iface
                    .constructors
                    .values()
                    .any(|vs| vs.iter().any(|v| v == unqualified)),
            };
            if !exported {
                return Err(UnresolvedKind::NotExported { module });
            }
        }
        if module == ["Prelude"] {
            Ok(Ref::Builtin(unqualified.to_string()))
        } else {
            Ok(Ref::Imported {
                module,
                name: unqualified.to_string(),
            })
        }
    }
}

fn exposed_ref(found: ExposedLookup, name: &str) -> Result<Ref, UnresolvedKind> {
    match found {
        ExposedLookup::Found(module) if module == ["Prelude"] => Ok(Ref::Builtin(name.to_string())),
        ExposedLookup::Found(module) => Ok(Ref::Imported {
            module,
            name: name.to_string(),
        }),
        ExposedLookup::Ambiguous(modules) => Err(UnresolvedKind::Ambiguous(modules)),
        ExposedLookup::NotFound => Err(UnresolvedKind::Unbound),
    }
}

/// Why a name didn't resolve -- callers (see `resolve/*.rs`) turn this into the
/// right `CanonErrorKind` variant, since the *kind* of name (variable vs.
/// constructor vs. type) determines which `CanonErrorKind` applies but the
/// lookup logic above is identical either way.
pub enum UnresolvedKind {
    Unbound,
    UnknownQualifier,
    Ambiguous(Vec<Vec<String>>),
    NotExported { module: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Registry {
        lookups: Cell<usize>,
        interface: RefCell<Option<ModuleInterface>>,
    }
    impl ModuleRegistry for Registry {
        fn exports(&self, _: &[String]) -> Option<ModuleInterface> {
            self.lookups.set(self.lookups.get() + 1);
            self.interface.borrow().clone()
        }
    }

    #[test]
    fn export_snapshot_preserves_namespaces_aliases_and_visibility_across_passes() {
        let module = knot_syntax::parse("module Main exposing (..)\nimport Library as A exposing (..)\nimport Library as B\nresult = A.value\n").unwrap();
        let registry = Registry {
            lookups: Cell::new(0),
            interface: RefCell::new(Some(ModuleInterface {
                values: ["value".into()].into(),
                types: ["Thing".into()].into(),
                constructors: [("Thing".into(), vec!["Make".into()])].into(),
            })),
        };
        let mut env = Env::for_module(&module.imports, Some(&registry));
        let imported = |name: &str| Ref::Imported {
            module: vec!["Library".into()],
            name: name.into(),
        };
        assert_eq!(env.resolve_value("A.value").ok(), Some(imported("value")));
        assert_eq!(env.resolve_value("B.value").ok(), Some(imported("value")));
        assert_eq!(env.resolve_type("A.Thing").ok(), Some(imported("Thing")));
        assert_eq!(env.resolve_ctor("B.Make").ok().unwrap().0, imported("Make"));
        assert!(matches!(
            env.resolve_value("A.hidden"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert!(matches!(
            env.resolve_value("A.Thing"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert!(matches!(
            env.resolve_type("B.value"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert!(matches!(
            env.resolve_ctor("A.Hidden"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert!(matches!(
            env.resolve_value("Other.value"),
            Err(UnresolvedKind::UnknownQualifier)
        ));
        env.declare_top_level_value("value");
        assert_eq!(
            env.resolve_value("value").ok(),
            Some(Ref::TopLevel("value".into()))
        );
        assert_eq!(
            registry.lookups.get(),
            1,
            "aliases and qualified uses share one export fetch"
        );

        registry.interface.borrow_mut().as_mut().unwrap().values = ["next".into()].into();
        let next = Env::for_module(&module.imports, Some(&registry));
        assert!(matches!(
            next.resolve_value("A.value"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert_eq!(next.resolve_value("B.next").ok(), Some(imported("next")));
        assert_eq!(env.resolve_value("A.value").ok(), Some(imported("value")));
        assert_eq!(
            registry.lookups.get(),
            2,
            "a new resolution pass fetches a new snapshot"
        );
    }

    #[test]
    fn qualified_imports_fetch_exports_only_when_used() {
        let module = knot_syntax::parse("module Main exposing (..)\nimport Library as A\nimport Library as B\nimport Unused\nresult = A.value\n").unwrap();
        let registry = Registry {
            lookups: Cell::new(0),
            interface: RefCell::new(Some(ModuleInterface {
                values: ["value".into()].into(),
                ..Default::default()
            })),
        };
        let env = Env::for_module(&module.imports, Some(&registry));
        assert_eq!(registry.lookups.get(), 0);
        assert!(matches!(
            env.resolve_value("Other.value"),
            Err(UnresolvedKind::UnknownQualifier)
        ));
        assert_eq!(registry.lookups.get(), 0);
        assert!(env.resolve_value("A.value").is_ok());
        assert!(env.resolve_value("B.value").is_ok());
        assert!(matches!(
            env.resolve_value("A.hidden"),
            Err(UnresolvedKind::NotExported { .. })
        ));
        assert_eq!(
            registry.lookups.get(),
            1,
            "unused imports need no export table"
        );
    }

    #[test]
    fn unknown_export_snapshot_retains_permissive_import_resolution() {
        let module = knot_syntax::parse("module Main exposing (..)\nimport Library as A exposing (..)\nimport Library as B\nresult = A.value\n").unwrap();
        let registry = Registry {
            lookups: Cell::new(0),
            interface: RefCell::new(None),
        };
        let env = Env::for_module(&module.imports, Some(&registry));
        assert!(env.resolve_value("A.value").is_ok());
        assert!(env.resolve_type("B.Thing").is_ok());
        assert!(env.resolve_ctor("A.Make").is_ok());
        assert!(env.resolve_value("value").is_ok());
        assert!(matches!(
            env.resolve_value("Other.value"),
            Err(UnresolvedKind::UnknownQualifier)
        ));
        assert_eq!(registry.lookups.get(), 1);
    }
}
