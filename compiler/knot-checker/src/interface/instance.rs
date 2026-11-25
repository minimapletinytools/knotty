//! Nominal v0 instances and the shared dictionary-evidence resolver.
//!
//! Unary heads are nominal constructors applied to distinct variables. Binary
//! interfaces match joint first-order patterns (`relation.rs`) and have unique
//! module-owned dictionary names. Custom structural patterns remain unsupported.
//! Checking, deriving, superclass validation and elaboration all use `resolve`;
//! `reduce` is just the residual-obligation view of that same evidence tree.

use std::collections::{HashMap, HashSet};

use knot_canonical::ast::{CConstraint, CDecl, CType, Ref};
use knot_syntax::span::Spanned;

use super::relation::{self, RelationInstance};
pub(crate) use super::relation::{dictionary_head, ordered_context};
use crate::ast::Dictionary;
use crate::error::{TypeError, TypeErrorKind};
use crate::interface::table::{is_known_interface, superclasses};
use crate::solve::PendingInstance;
use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceKind {
    BuiltIn,
    Declared,
}

/// Context obligations in deterministic dictionary-argument order. Each pair
/// names a leading type argument and the interface it must satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceEntry {
    pub kind: InstanceKind,
    pub requires: Vec<(usize, String)>,
    pub relation: Option<RelationInstance>,
}

#[derive(Debug, Default, Clone)]
pub struct InstanceTable {
    entries: HashMap<(String, Ref), InstanceEntry>,
}

impl InstanceTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn import_entries(
        &mut self,
        entries: &[(String, Ref, InstanceEntry)],
    ) -> Vec<TypeError> {
        let mut errors = Vec::new();
        for (interface, head, entry) in entries {
            let key = (interface.clone(), head.clone());
            if entry
                .relation
                .as_ref()
                .is_some_and(|r| self.overlaps(interface, &r.targets))
            {
                errors.push(TypeError {
                    span: knot_syntax::span::Span::new(0, 0),
                    kind: TypeErrorKind::DuplicateInstance {
                        interface: interface.clone(),
                    },
                });
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = self.entries.entry(key) {
                slot.insert(entry.clone());
            } else {
                errors.push(TypeError {
                    span: knot_syntax::span::Span::new(0, 0),
                    kind: TypeErrorKind::DuplicateInstance {
                        interface: interface.clone(),
                    },
                });
            }
        }
        errors
    }

    pub(crate) fn exported_entry(&self, interface: &str, head: &Ref) -> InstanceEntry {
        self.entry(interface, head)
            .expect("checked instance")
            .clone()
    }

    pub(crate) fn insert_relation(
        &mut self,
        interface: &str,
        head: Ref,
        targets: Vec<CType>,
        context: Vec<CConstraint>,
        kind: InstanceKind,
    ) {
        let context = ordered_context(&targets, &context);
        self.entries.insert(
            (interface.into(), head),
            InstanceEntry {
                kind,
                requires: Vec::new(),
                relation: Some(RelationInstance { targets, context }),
            },
        );
    }

    fn overlaps(&self, interface: &str, targets: &[CType]) -> bool {
        self.entries.iter().any(|((name, _), entry)| {
            name == interface
                && entry
                    .relation
                    .as_ref()
                    .is_some_and(|r| relation::overlaps(&r.targets, targets))
        })
    }

    pub fn has_instance(&self, interface: &str, head: &Ref) -> bool {
        self.entry(interface, head).is_some()
    }

    /// Finalized dictionary context, in argument order.
    pub fn requirements(&self, interface: &str, head: &Ref) -> Option<&[(usize, String)]> {
        self.entry(interface, head)
            .map(|entry| entry.requires.as_slice())
    }

    fn entry(&self, interface: &str, head: &Ref) -> Option<&InstanceEntry> {
        self.entries.get(&(interface.to_string(), head.clone()))
    }

    pub fn insert_builtin(&mut self, interface: &str, head: Ref, requires: Vec<(usize, String)>) {
        self.entries.insert(
            (interface.to_string(), head),
            InstanceEntry {
                kind: InstanceKind::BuiltIn,
                requires,
                relation: None,
            },
        );
    }

    /// Coherence has already been checked when user declarations are merged.
    pub fn merge_from(&mut self, other: InstanceTable) {
        for (key, entry) in other.entries {
            self.entries.entry(key).or_insert(entry);
        }
    }

    fn insert_declared_unchecked(
        &mut self,
        interface: &str,
        head: Ref,
        requires: Vec<(usize, String)>,
    ) {
        self.entries.insert(
            (interface.to_string(), head),
            InstanceEntry {
                kind: InstanceKind::Declared,
                requires,
                relation: None,
            },
        );
    }
}

pub(crate) fn instance_requires(
    target: &CType,
    constraints: &[CConstraint],
) -> Vec<(usize, String)> {
    let CType::Named(_, args) = target else {
        return Vec::new();
    };
    let mut requires = Vec::new();
    for (pos, arg) in args.iter().enumerate() {
        if let CType::Var(name) = arg {
            for c in constraints {
                if c.type_vars == [name.clone()] && !requires.contains(&(pos, c.interface.clone()))
                {
                    requires.push((pos, c.interface.clone()));
                }
            }
        }
    }
    requires
}

/// A nominal alias has already expanded here. Constructor-interface heads
/// may be partially applied, but each supplied argument is still a distinct
/// variable. Reject unsupported heads rather than silently dropping them.
fn instance_head(
    interface: &str,
    target: &CType,
    constraints: &[CConstraint],
) -> Result<(Ref, Vec<String>), TypeErrorKind> {
    let CType::Named(head, args) = target else {
        return Err(TypeErrorKind::InstanceTargetNotNominal {
            interface: interface.to_string(),
        });
    };
    let mut vars = Vec::new();
    for arg in args {
        match arg {
            CType::Var(name) if !vars.contains(name) => vars.push(name.clone()),
            _ => return Err(TypeErrorKind::UnsupportedInstanceHead {
                reason: "v0 instance arguments must be distinct type variables; specialized, nested and repeated arguments are unsupported".to_string(),
            }),
        }
    }
    if constraints
        .iter()
        .flat_map(|c| &c.type_vars)
        .any(|v| !vars.contains(v))
    {
        return Err(TypeErrorKind::UnsupportedInstanceHead {
            reason: "every instance-context variable must occur in the instance head".to_string(),
        });
    }
    Ok((head.clone(), vars))
}

struct DeclaredInstance<'a> {
    interface: &'a str,
    head: Ref,
    params: Vec<String>,
    span: knot_syntax::span::Span,
}

const DERIVABLE_INTERFACES: &[&str] = &["Eq", "Ord", "Show", "Semigroup", "Monoid"];

/// One `deriving` request that passed the shape-independent checks.
struct Derivation<'a> {
    interface: &'a str,
    type_name: &'a str,
    params: &'a [String],
    variants: &'a [(String, Vec<CType>)],
    span: knot_syntax::span::Span,
}

/// Collects every `deriving (...)` request, rejecting those that can't be
/// derived whatever the fields are: an interface outside
/// `DERIVABLE_INTERFACES`, or `Semigroup`/`Monoid` on a type with more than
/// one constructor (there's no sane `<>` between different constructors).
/// What a derived instance requires of its type's parameters is worked out
/// later, by `infer_derived_requirements`.
fn derive_instances<'a>(
    decls: &'a [Spanned<CDecl>],
    errors: &mut Vec<TypeError>,
) -> Vec<Derivation<'a>> {
    let mut derivations = Vec::new();
    for d in decls {
        let CDecl::TypeDecl(type_name, params, variants, deriving) = &d.node else {
            continue;
        };
        for interface in deriving {
            if !is_known_interface(interface) {
                // Already reported as UnknownInterface during canonicalization.
                continue;
            }
            let reason = if !DERIVABLE_INTERFACES.contains(&interface.as_str()) {
                Some(format!(
                    "{interface} cannot be derived automatically -- write the instance by hand"
                ))
            } else if matches!(interface.as_str(), "Semigroup" | "Monoid") && variants.len() != 1 {
                Some(format!(
                    "{interface} can only be derived for a type with exactly one constructor, \
                     not {}",
                    variants.len()
                ))
            } else {
                None
            };
            match reason {
                Some(reason) => errors.push(TypeError {
                    span: d.span,
                    kind: TypeErrorKind::CannotDeriveInterface {
                        interface: interface.clone(),
                        type_name: type_name.clone(),
                        reason,
                    },
                }),
                None => derivations.push(Derivation {
                    interface,
                    type_name,
                    params,
                    variants,
                    span: d.span,
                }),
            }
        }
    }
    derivations
}

/// Works out each derived instance's context — what it requires of its
/// type's parameters — the way a hand-written instance's body would need:
/// every field of every constructor must have the interface. Each field's
/// type is built with the type's parameters as unknowns and reduced
/// against the instance table; a residual on a parameter becomes a
/// requirement (`Wrap (List a)` deriving `Eq` requires `Eq a`), and a
/// failure is a `CannotDeriveInterface` naming the field.
///
/// Derived instances can depend on each other (and on themselves), so this
/// repeats until no requirement changes, starting from every derived
/// instance requiring nothing. Requirements only grow, and are bounded by
/// parameters times interfaces, so it terminates.
fn infer_derived_requirements(
    derivations: &[Derivation],
    accepted: &[bool],
    table: &mut InstanceTable,
    builtins: &InstanceTable,
    errors: &mut Vec<TypeError>,
) {
    let mut working = table.clone();
    working.merge_from(builtins.clone());
    let no_given = HashMap::new();
    let mut sub = Substitution::new();
    let mut failures: Vec<Option<String>> = vec![None; derivations.len()];
    loop {
        let mut changed = false;
        for (i, d) in derivations.iter().enumerate() {
            if !accepted[i] {
                continue;
            }
            let head = Ref::TopLevel(d.type_name.to_string());
            let mut requires = std::collections::BTreeSet::new();
            failures[i] = None;
            'fields: for (ctor, fields) in d.variants {
                let mut vars = HashMap::new();
                let params: Vec<TypeVarId> = d
                    .params
                    .iter()
                    .map(|p| {
                        let v = sub.fresh_unbound();
                        vars.insert(p.clone(), v);
                        v
                    })
                    .collect();
                for field in fields {
                    let ty =
                        crate::constrain::decl::instantiate_flexible(&mut sub, field, &mut vars);
                    // Every residual must land on one of the type's own
                    // parameters; anything else can't be stated as a context.
                    let ok = match reduce(&mut sub, &working, &no_given, d.interface, ty) {
                        Ok(residuals) => residuals.into_iter().all(|(iface, r)| match r {
                            Residual::Var(v) => {
                                let v = sub.find(v);
                                match params.iter().position(|p| sub.find(*p) == v) {
                                    Some(pos) => {
                                        requires.insert((pos, iface));
                                        true
                                    }
                                    None => false,
                                }
                            }
                            Residual::Stuck(_) | Residual::Relation(_) => false,
                        }),
                        Err(_) => false,
                    };
                    if !ok {
                        let field_ty = crate::print::print_type(&mut sub, ty);
                        failures[i] = Some(format!(
                            "constructor `{ctor}`'s field `{field_ty}` has no {} instance",
                            d.interface
                        ));
                        break 'fields;
                    }
                }
            }
            let requires: Vec<(usize, String)> = requires.into_iter().collect();
            let current = working
                .entry(d.interface, &head)
                .map(|e| e.requires.clone());
            if current.as_ref() != Some(&requires) {
                working.insert_declared_unchecked(d.interface, head.clone(), requires.clone());
                table.insert_declared_unchecked(d.interface, head, requires);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (d, failure) in derivations.iter().zip(failures) {
        if let Some(reason) = failure {
            errors.push(TypeError {
                span: d.span,
                kind: TypeErrorKind::CannotDeriveInterface {
                    interface: d.interface.to_string(),
                    type_name: d.type_name.to_string(),
                    reason,
                },
            });
        }
    }
}

pub fn build_instance_table(
    decls: &[Spanned<CDecl>],
    builtins: &InstanceTable,
) -> (InstanceTable, Vec<TypeError>) {
    let mut table = InstanceTable::new();
    let mut errors = Vec::new();
    let mut declared = Vec::new();
    let mut relations = Vec::new();
    for d in decls {
        let CDecl::Instance(inst) = &d.node else {
            continue;
        };
        if !is_known_interface(&inst.interface) {
            continue;
        }
        let expected = crate::interface::table::interface(&inst.interface)
            .unwrap()
            .vars
            .len();
        if inst.targets.len() != expected {
            continue;
        } // kind checker reports arity
        if expected > 1 || inst.constraints.iter().any(|c| c.type_vars.len() > 1) {
            let validation = if expected == 1 {
                instance_head(&inst.interface, &inst.targets[0], &inst.constraints)
                    .map(|_| ())
                    .and_then(|_| relation::validate(&inst.targets, &inst.constraints))
            } else {
                relation::validate(&inst.targets, &inst.constraints)
            };
            if let Err(kind) = validation {
                errors.push(TypeError { span: d.span, kind });
                continue;
            }
            let head = dictionary_head(&inst.interface, &inst.targets);
            if table.has_instance(&inst.interface, &head)
                || builtins.has_instance(&inst.interface, &head)
                || table.overlaps(&inst.interface, &inst.targets)
                || builtins.overlaps(&inst.interface, &inst.targets)
            {
                errors.push(TypeError {
                    span: d.span,
                    kind: TypeErrorKind::DuplicateInstance {
                        interface: inst.interface.clone(),
                    },
                });
            } else {
                table.insert_relation(
                    &inst.interface,
                    head,
                    inst.targets.clone(),
                    inst.constraints.clone(),
                    InstanceKind::Declared,
                );
                relations.push((inst, d.span));
            }
            continue;
        }
        match instance_head(&inst.interface, &inst.targets[0], &inst.constraints) {
            Ok((head, params)) => {
                declared.push((
                    DeclaredInstance {
                        interface: &inst.interface,
                        head,
                        params,
                        span: d.span,
                    },
                    instance_requires(&inst.targets[0], &inst.constraints),
                ));
            }
            Err(kind) => {
                errors.push(TypeError { span: d.span, kind });
            }
        }
    }
    let derivations = derive_instances(decls, &mut errors);
    let first_derived = declared.len();
    declared.extend(derivations.iter().map(|d| {
        (
            DeclaredInstance {
                interface: d.interface,
                head: Ref::TopLevel(d.type_name.to_string()),
                params: d.params.to_vec(),
                span: d.span,
            },
            Vec::new(),
        )
    }));

    let mut accepted = vec![false; declared.len()];
    for (i, (d, requires)) in declared.iter().enumerate() {
        if table.has_instance(d.interface, &d.head) || builtins.has_instance(d.interface, &d.head) {
            errors.push(TypeError {
                span: d.span,
                kind: TypeErrorKind::DuplicateInstance {
                    interface: d.interface.to_string(),
                },
            });
        } else {
            table.insert_declared_unchecked(d.interface, d.head.clone(), requires.clone());
            accepted[i] = true;
        }
    }

    // Deriving must finish first: superclass proofs need the final contexts,
    // including contexts inferred through mutually recursive declarations.
    infer_derived_requirements(
        &derivations,
        &accepted[first_derived..],
        &mut table,
        builtins,
        &mut errors,
    );
    let mut working = table.clone();
    working.merge_from(builtins.clone());
    // A unary instance may itself require a binary relation. Its superclass
    // proof must use that whole context just as its method bodies do.
    for (inst, span) in relations {
        if superclasses(&inst.interface).is_empty() {
            continue;
        }
        let mut sub = Substitution::new();
        let mut vars = HashMap::new();
        let targets = inst
            .targets
            .iter()
            .map(|t| crate::constrain::decl::instantiate_rigid(&mut sub, t, &mut vars))
            .collect();
        let target = crate::predicate::pack(&mut sub, targets);
        let mut given: HashMap<TypeVarId, HashSet<String>> = HashMap::new();
        for c in &inst.constraints {
            let args = c.type_vars.iter().map(|v| vars[v]).collect();
            given
                .entry(crate::predicate::pack(&mut sub, args))
                .or_default()
                .insert(c.interface.clone());
        }
        for superclass in superclasses(&inst.interface) {
            if !check_instance(&mut sub, &working, &given, superclass, target) {
                errors.push(TypeError {
                    span,
                    kind: TypeErrorKind::MissingSuperclassInstance {
                        interface: inst.interface.clone(),
                        superclass: superclass.clone(),
                    },
                });
            }
        }
    }
    for (i, (d, _)) in declared.iter().enumerate() {
        if !accepted[i] {
            continue;
        }
        let mut sub = Substitution::new();
        let args: Vec<_> = d
            .params
            .iter()
            .map(|p| sub.fresh_rigid(p.clone()))
            .collect();
        let mut given: HashMap<TypeVarId, HashSet<String>> = HashMap::new();
        for (pos, interface) in &table.entry(d.interface, &d.head).unwrap().requires {
            given
                .entry(args[*pos])
                .or_default()
                .insert(interface.clone());
        }
        let ty = sub.fresh_bound(Structure::App(d.head.clone(), args));
        for superclass in superclasses(d.interface) {
            // Do not assume the instance under construction as a given: doing
            // so would prove its own superclass circularly.
            if !check_instance(&mut sub, &working, &given, superclass, ty) {
                errors.push(TypeError {
                    span: d.span,
                    kind: TypeErrorKind::MissingSuperclassInstance {
                        interface: d.interface.to_string(),
                        superclass: superclass.clone(),
                    },
                });
            }
        }
    }
    (table, errors)
}

/// Interfaces every tuple, record, and `()` gets structurally, from its
/// parts, with no declaration (spec §10.4).
pub const STRUCTURAL_INTERFACES: &[&str] = &["Eq", "Ord", "Show", "Default"];

fn is_structural(interface: &str) -> bool {
    STRUCTURAL_INTERFACES.contains(&interface)
}

/// What is left of an obligation once everything known about its type has
/// been used.
#[derive(Debug, Clone, PartialEq)]
pub enum Residual {
    /// `interface` on a still-unknown type variable (the variable's root) —
    /// a unary scheme constraint.
    Var(TypeVarId),
    /// An unresolved multi-parameter obligation, retained as one predicate.
    Relation(TypeVarId),
    /// `interface` on a type whose head isn't known yet but isn't a bare
    /// variable either: an application of an unknown constructor variable
    /// (`f a`), or a record whose remaining fields are still open. It can
    /// only be discharged once that part of the type is solved.
    Stuck(TypeVarId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReduceError {
    /// No instance provides `interface` for `ty`.
    NoInstance { interface: String, ty: TypeVarId },
    /// A rigid variable was asked for an interface its signature (or
    /// instance context) doesn't grant.
    NotGiven { interface: String },
}

/// A proof tree used by both checking and elaboration. Residual leaves are
/// explicit: they must become binding parameters or be discharged before core
/// lowering. A `Given` identifies the original constraint, not its closure.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    Instance {
        dictionary: Dictionary,
        args: Vec<Evidence>,
    },
    Given {
        interface: String,
        ty: TypeVarId,
    },
    Super {
        from: Box<Evidence>,
        interface: String,
    },
    Structural {
        interface: String,
        shape: StructuralShape,
        args: Vec<Evidence>,
    },
    Residual {
        interface: String,
        obligation: Residual,
    },
}

#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub enum StructuralShape {
    Tuple(usize),
    Record(Vec<String>),
    Unit,
}

impl Evidence {
    pub fn residuals(&self) -> Vec<(String, Residual)> {
        let mut out = Vec::new();
        self.collect_residuals(&mut out);
        out
    }

    fn collect_residuals(&self, out: &mut Vec<(String, Residual)>) {
        match self {
            Self::Residual {
                interface,
                obligation,
            } => out.push((interface.clone(), obligation.clone())),
            Self::Instance { args, .. } | Self::Structural { args, .. } => {
                for arg in args {
                    arg.collect_residuals(out);
                }
            }
            Self::Super { from, .. } => from.collect_residuals(out),
            Self::Given { .. } => {}
        }
    }
}

/// A deterministic path through the closed, acyclic prelude superclass graph.
fn superclass_path(from: &str, to: &str) -> Option<Vec<String>> {
    if from == to {
        return Some(Vec::new());
    }
    for superclass in superclasses(from) {
        if let Some(mut tail) = superclass_path(superclass, to) {
            tail.insert(0, superclass.clone());
            return Some(tail);
        }
    }
    None
}

/// Resolve an obligation once, preserving all context and superclass evidence.
/// Unary contexts recurse into type arguments; binary contexts satisfy the
/// checked decreasing-size rule. Overlap is rejected during registration, and
/// matching here never unifies caller types to choose an instance.
pub fn resolve(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
) -> Result<Evidence, ReduceError> {
    let root = sub.find(ty);
    let no_instance = || ReduceError::NoInstance {
        interface: interface.to_string(),
        ty: root,
    };
    let given_root = if given.contains_key(&root) {
        Some(root)
    } else {
        given
            .keys()
            .copied()
            .filter(|v| crate::predicate::same(sub, *v, root))
            .min()
    };
    if let Some(given_root) = given_root {
        let givens = &given[&given_root];
        // Prefer the exact parameter, then deterministic superclass paths.
        let mut sources: Vec<_> = givens.iter().collect();
        sources.sort_by_key(|source| (*source != interface, *source));
        for source in sources {
            if let Some(path) = superclass_path(source, interface) {
                let mut evidence = Evidence::Given {
                    interface: source.clone(),
                    ty: given_root,
                };
                for superclass in path {
                    evidence = Evidence::Super {
                        from: Box::new(evidence),
                        interface: superclass,
                    };
                }
                return Ok(evidence);
            }
        }
    }
    if sub.is_rigid(root) {
        return Err(ReduceError::NotGiven {
            interface: interface.to_string(),
        });
    }
    let residual = |obligation| {
        Ok(Evidence::Residual {
            interface: interface.to_string(),
            obligation,
        })
    };
    let Some(structure) = sub.head_normal(root) else {
        return residual(Residual::Var(root));
    };
    let structural = |shape, args| {
        Ok(Evidence::Structural {
            interface: interface.to_string(),
            shape,
            args,
        })
    };
    match structure {
        Structure::Predicate(args) => resolve_relation(sub, table, given, interface, root, &args),
        Structure::App(head, args) | Structure::Ctor(head, args) => {
            let entry = table.entry(interface, &head).ok_or_else(no_instance)?;
            if entry.relation.is_some() {
                return resolve_relation(sub, table, given, interface, root, &[root]);
            }
            let mut evidence = Vec::new();
            for (pos, req) in &entry.requires {
                // A malformed type can reach here after kind checking failed.
                let arg = args.get(*pos).ok_or_else(no_instance)?;
                evidence.push(resolve(sub, table, given, req, *arg)?);
            }
            Ok(Evidence::Instance {
                dictionary: Dictionary {
                    interface: interface.to_string(),
                    head,
                },
                args: evidence,
            })
        }
        Structure::VarApp(f, _) => {
            if sub.is_rigid(f) {
                Err(no_instance())
            } else {
                residual(Residual::Stuck(root))
            }
        }
        Structure::Tuple(elems) if is_structural(interface) => {
            let arity = elems.len();
            let args = elems
                .into_iter()
                .map(|e| resolve(sub, table, given, interface, e))
                .collect::<Result<_, _>>()?;
            structural(StructuralShape::Tuple(arity), args)
        }
        Structure::Record(fields, ext) if is_structural(interface) => {
            let (fields, tail) = flatten_record(sub, fields, ext);
            if let Some(tail) = tail {
                return if sub.is_rigid(tail) {
                    Err(no_instance())
                } else {
                    residual(Residual::Stuck(root))
                };
            }
            let names = fields.keys().cloned().collect();
            let args = fields
                .into_values()
                .map(|t| resolve(sub, table, given, interface, t))
                .collect::<Result<_, _>>()?;
            structural(StructuralShape::Record(names), args)
        }
        Structure::Unit if is_structural(interface) => {
            structural(StructuralShape::Unit, Vec::new())
        }
        Structure::Unit | Structure::Fn(..) | Structure::Record(..) | Structure::Tuple(_) => {
            Err(no_instance())
        }
    }
}

fn resolve_relation(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
    args: &[TypeVarId],
) -> Result<Evidence, ReduceError> {
    for ((name, head), entry) in &table.entries {
        if name != interface {
            continue;
        }
        let Some(relation) = &entry.relation else {
            continue;
        };
        let Some(vars) = relation::matches(sub, &relation.targets, args) else {
            continue;
        };
        let mut evidence = Vec::new();
        for c in &relation.context {
            let args = c.type_vars.iter().map(|v| vars[v]).collect();
            let obligation = crate::predicate::pack(sub, args);
            evidence.push(resolve(sub, table, given, &c.interface, obligation)?);
        }
        return Ok(Evidence::Instance {
            dictionary: Dictionary {
                interface: interface.into(),
                head: head.clone(),
            },
            args: evidence,
        });
    }
    // Nominal relations run first: in particular, Constriction t t keeps exact
    // equality (and its Eq requirement), with no structural fallback on error.
    // Structural relations are compiler-owned, so they cannot overlap a user
    // instance: custom tuple/record heads remain forbidden.
    if let ("Constriction", [constraint, value]) = (interface, args) {
        let parts = match (sub.head_normal(*constraint), sub.head_normal(*value)) {
            (Some(Structure::Tuple(xs)), Some(Structure::Tuple(ys))) => {
                if xs.len() != ys.len() {
                    return Err(ReduceError::NoInstance {
                        interface: interface.into(),
                        ty,
                    });
                }
                Some((
                    StructuralShape::Tuple(xs.len()),
                    xs.into_iter().zip(ys).collect::<Vec<_>>(),
                ))
            }
            (Some(Structure::Record(xs, x)), Some(Structure::Record(ys, y))) => {
                let (xs, x) = flatten_record(sub, xs, x);
                let (ys, y) = flatten_record(sub, ys, y);
                if x.is_some() || y.is_some() {
                    // Like structural Eq, wait for closed rows rather than
                    // inferring a row constraint or silently dropping fields.
                    return Ok(Evidence::Residual {
                        interface: interface.into(),
                        obligation: Residual::Stuck(ty),
                    });
                }
                if !xs.keys().eq(ys.keys()) {
                    return Err(ReduceError::NoInstance {
                        interface: interface.into(),
                        ty,
                    });
                }
                Some((
                    StructuralShape::Record(xs.keys().cloned().collect()),
                    xs.into_values().zip(ys.into_values()).collect(),
                ))
            }
            _ => None,
        };
        if let Some((shape, parts)) = parts {
            let mut evidence = Vec::with_capacity(parts.len());
            for (constraint, value) in parts {
                let pair = crate::predicate::pack(sub, vec![constraint, value]);
                evidence.push(resolve(sub, table, given, interface, pair)?);
            }
            return Ok(Evidence::Structural {
                interface: interface.into(),
                shape,
                args: evidence,
            });
        }
    }
    if !crate::solve::free_vars(sub, ty).is_empty() {
        Ok(Evidence::Residual {
            interface: interface.into(),
            obligation: Residual::Relation(ty),
        })
    } else {
        Err(ReduceError::NoInstance {
            interface: interface.into(),
            ty,
        })
    }
}

/// The solver and deriving consume the residual view of the same proof that
/// elaboration consumes; there is no second instance-selection algorithm.
pub fn reduce(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
) -> Result<Vec<(String, Residual)>, ReduceError> {
    resolve(sub, table, given, interface, ty).map(|e| e.residuals())
}

/// A record's fields with any record bound into its extension merged in,
/// plus the remaining open tail (`None` once closed).
pub(crate) fn flatten_record(
    sub: &mut Substitution,
    mut fields: std::collections::BTreeMap<String, TypeVarId>,
    mut ext: Option<TypeVarId>,
) -> (
    std::collections::BTreeMap<String, TypeVarId>,
    Option<TypeVarId>,
) {
    while let Some(e) = ext {
        match sub.resolve_structure(e) {
            Some(Structure::Record(more, next)) => {
                fields.extend(more);
                ext = next;
            }
            _ => return (fields, Some(sub.find(e))),
        }
    }
    (fields, None)
}

/// Does `ty` have `interface` outright, with nothing left over?
pub fn check_instance(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
) -> bool {
    matches!(reduce(sub, table, given, interface, ty), Ok(r) if r.is_empty())
}

/// Checks a list of obligations against `table`, reporting the ones that
/// definitely fail. Anything that only reduces to residuals is left alone.
pub fn check_pending(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    pending: Vec<PendingInstance>,
    errors: &mut Vec<TypeError>,
) {
    for p in pending {
        if let Err(e) = reduce(sub, table, given, &p.interface, p.ty) {
            errors.push(TypeError {
                span: p.span,
                kind: reduce_error_kind(sub, e),
            });
        }
    }
}

pub fn reduce_error_kind(sub: &mut Substitution, e: ReduceError) -> TypeErrorKind {
    match e {
        ReduceError::NoInstance { interface, ty } => TypeErrorKind::NoInstance {
            interface,
            ty: crate::print::print_type(sub, ty),
        },
        ReduceError::NotGiven { interface } => TypeErrorKind::NoInstanceForRigid { interface },
    }
}

/// Every instance's method list against its interface: no unknown methods,
/// none defined twice, none missing.
pub fn check_instance_methods(decls: &[Spanned<CDecl>]) -> Vec<TypeError> {
    let mut errors = Vec::new();
    for d in decls {
        let CDecl::Instance(inst) = &d.node else {
            continue;
        };
        let Some(iface) = crate::interface::table::interface(&inst.interface) else {
            continue;
        };
        let mut seen = HashSet::new();
        for method in &inst.methods {
            let kind = if !iface.methods.iter().any(|(m, _)| *m == method.name) {
                TypeErrorKind::UnknownInstanceMethod {
                    interface: inst.interface.clone(),
                    method: method.name.clone(),
                }
            } else if !seen.insert(method.name.as_str()) {
                TypeErrorKind::DuplicateInstanceMethod {
                    interface: inst.interface.clone(),
                    method: method.name.clone(),
                }
            } else {
                continue;
            };
            errors.push(TypeError { span: d.span, kind });
        }
        // The two Show defaults delegate to each other. Accept either
        // explicit implementation, but never a dictionary made only of them.
        // This is the v0 minimum rule, not a general MINIMAL language.
        if inst.interface == "Show" && !seen.contains("show") && !seen.contains("showsPrec") {
            errors.push(TypeError {
                span: d.span,
                kind: TypeErrorKind::MissingMinimalInstanceMethods {
                    interface: inst.interface.clone(),
                    alternatives: vec!["show".into(), "showsPrec".into()],
                },
            });
        }
        for (m, _) in &iface.methods {
            if !iface.defaults.contains(m) && !inst.methods.iter().any(|method| method.name == *m) {
                errors.push(TypeError {
                    span: d.span,
                    kind: TypeErrorKind::MissingInstanceMethod {
                        interface: inst.interface.clone(),
                        method: m.clone(),
                    },
                });
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_instances_are_trusted_unconditionally() {
        let mut table = InstanceTable::new();
        table.insert_builtin("Num", Ref::Builtin("Int".to_string()), vec![]);
        assert!(table.has_instance("Num", &Ref::Builtin("Int".to_string())));
        assert!(!table.has_instance("Num", &Ref::Builtin("String".to_string())));
    }

    fn decls(src: &str) -> Vec<Spanned<CDecl>> {
        let mut state = knot_syntax::ParseState::new(src);
        let raw = state.parse_decls().unwrap();
        assert!(state.is_eof(), "leftover input: {src}");
        knot_canonical::canonicalize_decls(&raw).unwrap_or_else(|errs| panic!("{errs:?}"))
    }

    #[test]
    fn a_non_generic_adt_can_derive_eq_ord_show() {
        let mut builtins = InstanceTable::new();
        builtins.insert_builtin("Eq", Ref::Builtin("Float".to_string()), vec![]);
        builtins.insert_builtin("Ord", Ref::Builtin("Float".to_string()), vec![]);
        builtins.insert_builtin("Show", Ref::Builtin("Float".to_string()), vec![]);
        let cs = decls(
            "type Shape\n  = Circle Float\n  | Rectangle Float Float\n  deriving (Eq, Ord, Show)\n",
        );
        let (table, errors) = build_instance_table(&cs, &builtins);
        assert!(errors.is_empty(), "{errors:?}");
        let shape = Ref::TopLevel("Shape".to_string());
        assert!(table.has_instance("Eq", &shape));
        assert!(table.has_instance("Ord", &shape));
        assert!(table.has_instance("Show", &shape));
    }

    #[test]
    fn a_generic_adt_derives_a_positional_requirement_for_each_parameter_used() {
        let cs = decls("type Box a = Box a\n  deriving (Eq)\n");
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");
        let entry = table
            .entry("Eq", &Ref::TopLevel("Box".to_string()))
            .expect("derived Eq Box should be registered");
        assert_eq!(entry.requires, vec![(0, "Eq".to_string())]);
    }

    #[test]
    fn a_recursive_adt_can_derive_show_via_a_canonical_self_reference() {
        let cs = decls("type Tree a\n  = Leaf\n  | Node (Tree a) a (Tree a)\n  deriving (Show)\n");
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");
        let entry = table
            .entry("Show", &Ref::TopLevel("Tree".to_string()))
            .expect("derived Show Tree should be registered");
        assert_eq!(entry.requires, vec![(0, "Show".to_string())]);
    }

    #[test]
    fn a_single_constructor_type_can_derive_semigroup_and_monoid() {
        let mut builtins = InstanceTable::new();
        builtins.insert_builtin("Semigroup", Ref::Builtin("String".to_string()), vec![]);
        builtins.insert_builtin("Monoid", Ref::Builtin("String".to_string()), vec![]);
        let cs = decls("type Wrapper = Wrap String\n  deriving (Semigroup, Monoid)\n");
        let (table, errors) = build_instance_table(&cs, &builtins);
        assert!(errors.is_empty(), "{errors:?}");
        let wrapper = Ref::TopLevel("Wrapper".to_string());
        assert!(table.has_instance("Semigroup", &wrapper));
        assert!(table.has_instance("Monoid", &wrapper));
    }

    #[test]
    fn deriving_semigroup_on_a_multi_constructor_type_is_rejected() {
        // There's no sane `<>` between two different constructors of a sum
        // type.
        let cs = decls("type Shape\n  = Circle Float\n  | Square Float\n  deriving (Semigroup)\n");
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, type_name, .. }
                if interface == "Semigroup" && type_name == "Shape"
        )));
    }

    #[test]
    fn deriving_num_is_rejected_even_though_its_only_field_already_has_num() {
        // Num is deliberately excluded from DERIVABLE_INTERFACES -- must be
        // rejected regardless of whether the field-shape scan would
        // otherwise have accepted it.
        let mut builtins = InstanceTable::new();
        builtins.insert_builtin("Num", Ref::Builtin("Int".to_string()), vec![]);
        let cs = decls("type MyNum = MyNum Int\n  deriving (Num)\n");
        let (_table, errors) = build_instance_table(&cs, &builtins);
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, .. } if interface == "Num"
        )));
    }

    fn prelude_table() -> InstanceTable {
        crate::prelude::seed(&mut Substitution::new()).1
    }

    fn derived_requires(src: &str, interface: &str, type_name: &str) -> Vec<(usize, String)> {
        let cs = decls(src);
        let (table, errors) = build_instance_table(&cs, &prelude_table());
        assert!(errors.is_empty(), "{errors:?}");
        table
            .entry(interface, &Ref::TopLevel(type_name.to_string()))
            .unwrap_or_else(|| panic!("derived {interface} {type_name} should be registered"))
            .requires
            .clone()
    }

    #[test]
    fn deriving_through_a_nested_generic_field_requires_the_parameter() {
        assert_eq!(
            derived_requires(
                "type Wrapper a = Wrap (List a)\n  deriving (Eq)\n",
                "Eq",
                "Wrapper"
            ),
            vec![(0, "Eq".to_string())]
        );
        assert_eq!(
            derived_requires(
                "type Pairs k v = Pairs (List (k, Maybe v))\n  deriving (Show)\n",
                "Show",
                "Pairs"
            ),
            vec![(0, "Show".to_string()), (1, "Show".to_string())]
        );
    }

    #[test]
    fn deriving_only_requires_the_parameters_fields_actually_use() {
        assert_eq!(
            derived_requires(
                "type Tagged t a = Tagged a\n  deriving (Eq)\n",
                "Eq",
                "Tagged"
            ),
            vec![(1, "Eq".to_string())]
        );
    }

    #[test]
    fn deriving_through_another_user_type_declared_later() {
        let src = "type Wrap = W Circle\n  deriving (Eq)\n\
                   type Circle = Circle Float\n  deriving (Eq)\n";
        assert!(derived_requires(src, "Eq", "Wrap").is_empty());
    }

    #[test]
    fn deriving_through_a_hand_written_instance() {
        let src = "type Circle = Circle Float\n\
                   instance Eq Circle where\n  (==) a b = True\n\
                   type Wrap = W Circle\n  deriving (Eq)\n";
        assert!(derived_requires(src, "Eq", "Wrap").is_empty());
    }

    #[test]
    fn mutually_recursive_types_derive_together() {
        let src = "type Forest a = Forest (List (Tree a))\n  deriving (Show)\n\
                   type Tree a = Node a (Forest a)\n  deriving (Show)\n";
        assert_eq!(
            derived_requires(src, "Show", "Forest"),
            vec![(0, "Show".to_string())]
        );
        assert_eq!(
            derived_requires(src, "Show", "Tree"),
            vec![(0, "Show".to_string())]
        );
    }

    #[test]
    fn deriving_through_records_and_tuples() {
        let src = "type Shape = Circle { cx : Float, r : Float } | Seg (Float, Float)\n  deriving (Eq, Ord, Show)\n";
        assert!(derived_requires(src, "Ord", "Shape").is_empty());
    }

    #[test]
    fn a_non_canonical_self_reference_requires_nothing_extra() {
        assert!(derived_requires(
            "type Weird a b = Weird (Weird b a)\n  deriving (Eq)\n",
            "Eq",
            "Weird"
        )
        .is_empty());
    }

    #[test]
    fn deriving_through_a_type_without_the_instance_names_the_field() {
        let cs = decls("type Plain = Plain\ntype Wrap = W Plain\n  deriving (Eq)\n");
        let (_table, errors) = build_instance_table(&cs, &prelude_table());
        assert!(
            errors.iter().any(|e| matches!(
                &e.kind,
                TypeErrorKind::CannotDeriveInterface { reason, .. }
                    if reason == "constructor `W`'s field `Plain` has no Eq instance"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn deriving_through_a_function_field_is_rejected() {
        let cs = decls("type Handler = Handler (Int -> Int)\n  deriving (Eq)\n");
        let (_table, errors) = build_instance_table(&cs, &prelude_table());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, .. } if interface == "Eq"
        )));
    }

    #[test]
    fn deriving_through_a_constructor_variable_application_is_rejected() {
        let cs = decls("type Wrapped f a = Wrapped (f a)\n  deriving (Eq)\n");
        let (_table, errors) = build_instance_table(&cs, &prelude_table());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, .. } if interface == "Eq"
        )));
    }

    #[test]
    fn deriving_with_a_concrete_field_that_has_no_instance_is_rejected() {
        let cs = decls("type Weird = Weird Int\n  deriving (Semigroup)\n");
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, .. } if interface == "Semigroup"
        )));
    }

    #[test]
    fn deriving_eq_on_a_unit_field_is_accepted() {
        let cs = decls("type Event = Clicked ()\n  deriving (Eq)\n");
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.has_instance("Eq", &Ref::TopLevel("Event".to_string())));
    }

    #[test]
    fn deriving_semigroup_on_a_unit_field_is_rejected() {
        // Unit only derives Eq/Ord/Show structurally, never Semigroup/Monoid
        // -- matches check_instance's own existing asymmetry for Unit.
        let cs = decls("type Event = Clicked ()\n  deriving (Semigroup)\n");
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::CannotDeriveInterface { interface, .. } if interface == "Semigroup"
        )));
    }

    #[test]
    fn a_declared_instance_with_its_superclass_already_present_is_accepted() {
        let cs = decls(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n\
             instance Ord Shape where\n  compare a b = EQ\n",
        );
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.has_instance("Eq", &Ref::TopLevel("Shape".to_string())));
        assert!(table.has_instance("Ord", &Ref::TopLevel("Shape".to_string())));
    }

    #[test]
    fn declaring_ord_without_eq_first_is_a_missing_superclass_error() {
        let cs = decls("type Shape = Circle Float\ninstance Ord Shape where\n  compare a b = EQ\n");
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.iter().any(|e| matches!(
            &e.kind,
            TypeErrorKind::MissingSuperclassInstance { interface, superclass }
                if interface == "Ord" && superclass == "Eq"
        )));
    }

    #[test]
    fn a_superclass_declared_after_its_own_subclass_in_the_same_module_is_still_accepted() {
        // Same two instances as
        // `a_declared_instance_with_its_superclass_already_present_is_accepted`,
        // just in the opposite declaration order -- coherence must not
        // depend on where in the module each instance happens to be
        // written.
        let cs = decls(
            "type Shape = Circle Float\n\
             instance Ord Shape where\n  compare a b = EQ\n\
             instance Eq Shape where\n  (==) a b = True\n",
        );
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.has_instance("Eq", &Ref::TopLevel("Shape".to_string())));
        assert!(table.has_instance("Ord", &Ref::TopLevel("Shape".to_string())));
    }

    #[test]
    fn a_duplicate_instance_with_a_genuinely_missing_superclass_reports_each_error_once() {
        // Two `Ord Shape`s (a duplicate) and no `Eq` at all -- the second,
        // rejected `Ord` shouldn't also get its own independent
        // MissingSuperclassInstance check (pass 2 only checks accepted
        // instances), so this should be exactly one DuplicateInstance and
        // exactly one MissingSuperclassInstance, not two of the latter.
        let cs = decls(
            "type Shape = Circle Float\n\
             instance Ord Shape where\n  compare a b = EQ\n\
             instance Ord Shape where\n  compare a b = EQ\n",
        );
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        let duplicates = errors
            .iter()
            .filter(|e| matches!(&e.kind, TypeErrorKind::DuplicateInstance { .. }))
            .count();
        let missing_superclass = errors
            .iter()
            .filter(|e| matches!(&e.kind, TypeErrorKind::MissingSuperclassInstance { .. }))
            .count();
        assert_eq!(duplicates, 1, "{errors:?}");
        assert_eq!(missing_superclass, 1, "{errors:?}");
    }

    #[test]
    fn declaring_the_same_instance_twice_is_a_duplicate_error() {
        let cs = decls(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n\
             instance Eq Shape where\n  (==) a b = False\n",
        );
        let (_table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors
            .iter()
            .any(|e| matches!(&e.kind, TypeErrorKind::DuplicateInstance { interface } if interface == "Eq")));
    }

    #[test]
    fn redeclaring_an_instance_a_builtin_type_already_has_is_a_duplicate_error() {
        // Task #40: build_instance_table's own coherence pass used to only
        // ever see a module's own declared instances, not the builtin
        // table it gets merged with only afterward (InstanceTable::
        // merge_from) -- so `instance Eq Int where ...` type-checked as if
        // Int had no Eq instance yet at all. Now `builtins` is consulted
        // too.
        let mut builtins = InstanceTable::new();
        builtins.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        let cs = decls("instance Eq Int where\n  (==) a b = False\n");
        let (table, errors) = build_instance_table(&cs, &builtins);
        assert!(errors
            .iter()
            .any(|e| matches!(&e.kind, TypeErrorKind::DuplicateInstance { interface } if interface == "Eq")));
        // Rejected, not inserted -- table itself stays empty for this head.
        assert!(!table.has_instance("Eq", &Ref::Builtin("Int".to_string())));
    }

    #[test]
    fn declaring_a_new_instance_for_a_builtin_type_is_still_accepted() {
        // The fix above must not become overly conservative -- a builtin
        // type genuinely getting a *new* interface it doesn't already have
        // (Semigroup has no builtin Int instance) is still fine.
        let mut builtins = InstanceTable::new();
        builtins.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        let cs = decls("instance Semigroup Int where\n  (<>) a b = a\n");
        let (table, errors) = build_instance_table(&cs, &builtins);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(table.has_instance("Semigroup", &Ref::Builtin("Int".to_string())));
    }

    #[test]
    fn check_pending_flags_a_concrete_type_with_no_instance() {
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let ty = sub.fresh_bound(Structure::App(Ref::Builtin("String".to_string()), vec![]));
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Num".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.iter().any(
            |e| matches!(&e.kind, TypeErrorKind::NoInstance { interface, .. } if interface == "Num")
        ));
    }

    #[test]
    fn check_pending_accepts_a_seeded_builtin_instance() {
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Num", Ref::Builtin("Int".to_string()), vec![]);
        let ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Num".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn check_pending_resolves_a_ctor_headed_obligation_against_the_table() {
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Functor", Ref::Builtin("List".to_string()), vec![]);
        let ty = sub.fresh_bound(Structure::Ctor(Ref::Builtin("List".to_string()), vec![]));
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Functor".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn check_pending_flags_a_ctor_headed_obligation_with_no_instance() {
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let ty = sub.fresh_bound(Structure::Ctor(Ref::Builtin("IO".to_string()), vec![]));
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Functor".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.iter().any(
            |e| matches!(&e.kind, TypeErrorKind::NoInstance { interface, .. } if interface == "Functor")
        ));
    }

    #[test]
    fn check_pending_leaves_an_unresolved_ctor_var_unflagged() {
        // A still-unbound constructor variable (never pinned to a concrete
        // head) is a kind error, not a missing-instance one -- left alone
        // here, same as any other still-unresolved obligation.
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let ty = sub.fresh_unbound();
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Functor".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn check_pending_leaves_a_still_unresolved_type_variable_unflagged() {
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let ty = sub.fresh_unbound();
        let pending = vec![PendingInstance {
            span: knot_syntax::span::Span::new(0, 0),
            interface: "Eq".to_string(),
            ty,
        }];
        let mut errors = Vec::new();
        check_pending(&mut sub, &table, &HashMap::new(), pending, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn empty_tuple_trivially_satisfies_eq_ord_show() {
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let ty = sub.fresh_bound(Structure::Tuple(vec![]));
        for interface in ["Eq", "Ord", "Show"] {
            assert!(check_instance(
                &mut sub,
                &table,
                &HashMap::new(),
                interface,
                ty
            ));
        }
    }

    #[test]
    fn a_function_is_never_eq_ord_or_show() {
        let mut sub = Substitution::new();
        let table = InstanceTable::new();
        let a = sub.fresh_unbound();
        let fn_ty = sub.fresh_bound(Structure::Fn(a, a));
        assert!(!check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            fn_ty
        ));
    }

    #[test]
    fn tuple_is_eq_only_if_every_element_is() {
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let weird_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Weird".to_string()), vec![]));

        let ok_pair = sub.fresh_bound(Structure::Tuple(vec![int_ty, int_ty]));
        assert!(check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            ok_pair
        ));

        let bad_pair = sub.fresh_bound(Structure::Tuple(vec![int_ty, weird_ty]));
        assert!(!check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            bad_pair
        ));
    }

    #[test]
    fn tuple_never_structurally_satisfies_a_non_derivable_interface() {
        // Knot has no tuple arithmetic -- (Int, Int) must not inherit Int's
        // own Num instance just because every element happens to have one.
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Num", Ref::Builtin("Int".to_string()), vec![]);
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let pair = sub.fresh_bound(Structure::Tuple(vec![int_ty, int_ty]));
        assert!(!check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Num",
            pair
        ));
    }

    #[test]
    fn record_is_eq_only_if_every_field_is() {
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let weird_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Weird".to_string()), vec![]));

        let mut ok_fields = std::collections::BTreeMap::new();
        ok_fields.insert("x".to_string(), int_ty);
        let ok_record = sub.fresh_bound(Structure::Record(ok_fields, None));
        assert!(check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            ok_record
        ));

        let mut bad_fields = std::collections::BTreeMap::new();
        bad_fields.insert("x".to_string(), weird_ty);
        let bad_record = sub.fresh_bound(Structure::Record(bad_fields, None));
        assert!(!check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            bad_record
        ));
    }

    #[test]
    fn a_parametric_builtin_instance_recurses_into_its_own_element_type() {
        // List Weird's Eq must fail even though List itself has a seeded Eq
        // entry -- the whole point of `requires`.
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        table.insert_builtin(
            "Eq",
            Ref::Builtin("List".to_string()),
            vec![(0, "Eq".to_string())],
        );
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let weird_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Weird".to_string()), vec![]));
        let list_int = sub.fresh_bound(Structure::App(
            Ref::Builtin("List".to_string()),
            vec![int_ty],
        ));
        let list_weird = sub.fresh_bound(Structure::App(
            Ref::Builtin("List".to_string()),
            vec![weird_ty],
        ));

        assert!(check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            list_int
        ));
        assert!(!check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            list_weird
        ));
    }

    #[test]
    fn nested_containers_resolve_two_levels_deep() {
        let mut sub = Substitution::new();
        let mut table = InstanceTable::new();
        table.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        table.insert_builtin(
            "Eq",
            Ref::Builtin("List".to_string()),
            vec![(0, "Eq".to_string())],
        );
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let list_int = sub.fresh_bound(Structure::App(
            Ref::Builtin("List".to_string()),
            vec![int_ty],
        ));
        let list_list_int = sub.fresh_bound(Structure::App(
            Ref::Builtin("List".to_string()),
            vec![list_int],
        ));
        assert!(check_instance(
            &mut sub,
            &table,
            &HashMap::new(),
            "Eq",
            list_list_int
        ));
    }

    #[test]
    fn a_declared_parametric_instance_populates_requires_from_its_own_constraints() {
        let cs = decls(
            "type Box a = Box a\n\
             instance Eq a => Eq (Box a) where\n  (==) x y = True\n",
        );
        let (table, errors) = build_instance_table(&cs, &InstanceTable::new());
        assert!(errors.is_empty(), "{errors:?}");

        let mut sub = Substitution::new();
        let int_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Int".to_string()), vec![]));
        let weird_ty = sub.fresh_bound(Structure::App(Ref::Builtin("Weird".to_string()), vec![]));
        let box_ref = Ref::TopLevel("Box".to_string());
        let box_int = sub.fresh_bound(Structure::App(box_ref.clone(), vec![int_ty]));
        let box_weird = sub.fresh_bound(Structure::App(box_ref, vec![weird_ty]));

        // Int itself needs its own seeded Eq for the recursion to succeed.
        let mut table_with_int = table;
        table_with_int.insert_builtin("Eq", Ref::Builtin("Int".to_string()), vec![]);
        assert!(check_instance(
            &mut sub,
            &table_with_int,
            &HashMap::new(),
            "Eq",
            box_int
        ));
        assert!(!check_instance(
            &mut sub,
            &table_with_int,
            &HashMap::new(),
            "Eq",
            box_weird
        ));
    }

    // -- end to end, through the real entry point --

    fn errors_of(src: &str) -> Vec<TypeErrorKind> {
        crate::check::check_source(src)
            .errors
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    fn has(errors: &[TypeErrorKind], prefix: &str) -> bool {
        errors.iter().any(|e| format!("{e:?}").starts_with(prefix))
    }

    #[test]
    fn a_user_declared_instance_satisfies_a_concrete_use() {
        let errors = errors_of(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n\
             compareShapes :: Shape -> Shape -> Bool\n\
             compareShapes a b = a == b\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_concrete_use_with_no_declared_instance_is_a_no_instance_error() {
        let errors = errors_of(
            "type Shape = Circle Float\n\
             compareShapes :: Shape -> Shape -> Bool\n\
             compareShapes a b = a == b\n",
        );
        assert!(
            has(&errors, "NoInstance { interface: \"Eq\", ty: \"Shape\""),
            "{errors:?}"
        );
    }

    #[test]
    fn an_instance_methods_body_returning_the_wrong_type_is_an_error() {
        let errors =
            errors_of("type Shape = Circle Float\ninstance Eq Shape where\n  (==) a b = 5\n");
        assert!(
            has(&errors, "NoInstance { interface: \"Num\", ty: \"Bool\""),
            "{errors:?}"
        );
    }

    #[test]
    fn a_parametric_instance_methods_body_may_use_its_own_context() {
        let errors = errors_of(
            "type Box a = Box a\n\
             instance Eq a => Eq (Box a) where\n  (==) (Box x) (Box y) = x == y\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_parametric_instance_body_using_more_than_its_context_grants_is_an_error() {
        let errors = errors_of(
            "type Box a = Box a\n\
             instance Eq a => Eq (Box a) where\n  (==) (Box x) (Box y) = x < y\n",
        );
        assert!(
            has(&errors, "NoInstanceForRigid { interface: \"Ord\""),
            "{errors:?}"
        );
    }

    #[test]
    fn unknown_missing_and_duplicate_methods_are_reported() {
        let errors = errors_of(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n  bogus x = x\n",
        );
        assert!(has(&errors, "UnknownInstanceMethod"), "{errors:?}");
        let errors =
            errors_of("type Shape = Circle Float\ninstance Num Shape where\n  (+) a b = a\n");
        assert!(has(
            &errors,
            "MissingInstanceMethod { interface: \"Num\", method: \"-\""
        ));
        let errors = errors_of(
            "type Shape = Circle Float\n\
             instance Eq Shape where\n  (==) a b = True\n  (==) a b = False\n",
        );
        assert!(has(&errors, "DuplicateInstanceMethod"), "{errors:?}");
    }

    #[test]
    fn functor_and_foldable_instance_methods_are_checked_against_their_signatures() {
        let errors = errors_of(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b =\n",
            "    case b of\n",
            "      Box x -> Box (f x)\n",
            "instance Foldable Box where\n",
            "  foldl f z b =\n",
            "    case b of\n",
            "      Box x -> f z x\n",
            "  foldr f z b =\n",
            "    case b of\n",
            "      Box x -> f x z\n",
            "  length b = 1\n",
        ));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn applicative_and_monad_instance_methods_are_checked_against_their_signatures() {
        let errors = errors_of(concat!(
            "type Box a = Box a\n",
            "instance Functor Box where\n",
            "  map f b =\n",
            "    case b of\n",
            "      Box x -> Box (f x)\n",
            "instance Applicative Box where\n",
            "  pure x = Box x\n",
            "  (<*>) bf bx =\n",
            "    case (bf, bx) of\n",
            "      (Box f, Box x) -> Box (f x)\n",
            "instance Monad Box where\n",
            "  (>>=) b f =\n",
            "    case b of\n",
            "      Box x -> f x\n",
        ));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_functor_method_that_ignores_its_function_is_rejected() {
        // `a` and `b` are rigid: returning the input `Box a` where `Box b`
        // is required must fail, not unify `a` with `b`.
        let errors = errors_of("type Box a = Box a\ninstance Functor Box where\n  map f b = b\n");
        assert!(has(&errors, "Mismatch"), "{errors:?}");
    }

    #[test]
    fn a_partially_applied_instance_target_is_checked() {
        let ok = errors_of(concat!(
            "type Outcome e a = Failure e | Success a\n",
            "instance Functor (Outcome e) where\n",
            "  map f o = case o of\n",
            "    Failure e -> Failure e\n",
            "    Success x -> Success (f x)\n",
        ));
        assert!(ok.is_empty(), "{ok:?}");
        let bad = errors_of(concat!(
            "type Outcome e a = Failure e | Success a\n",
            "instance Functor (Outcome e) where\n",
            "  map f o = o\n",
        ));
        assert!(has(&bad, "Mismatch"), "{bad:?}");
    }
}
