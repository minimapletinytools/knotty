//! `knot-checker` — type-checks a `knot-canonical` `CModule`.
//!
//! **Pipeline** (`check::check_module_report`):
//! 1. `prelude::seed` builds the scheme environment and instance table from
//!    `knot-canonical`'s library modules (`lib/Prelude.knot`, `lib/Map.knot`),
//!    the single declaration of every builtin type, constructor, interface,
//!    instance, value, and operator. `check::check_library` checks their
//!    bodies.
//! 2. `kind::check_kinds` checks every type expression is well-kinded.
//! 3. `interface::instance::build_instance_table` registers the module's
//!    instances (coherence, superclasses, `deriving`), and
//!    `check_instance_methods` checks each instance's method list.
//! 4. `constrain::decl::constrain_module` generates a `Constraint` tree:
//!    bindings split into dependency groups (SCCs), each a nested
//!    `Constraint::Let`; instance methods checked against their interface's
//!    declared signatures.
//! 5. `solve::solve_module` solves it: unification (`unify.rs`),
//!    level-based generalization (`var.rs`), and interface obligations
//!    resolved through `interface::instance::resolve`. Its residual view,
//!    `reduce`, supplies obligations to quantify, default, or report.
//! 6. `exhaustiveness` reports non-exhaustive and redundant `case`s as
//!    warnings.
//!
//! `print` renders types and schemes as Knot text, for error messages and
//! for the corpus's inferred-type golden tests. `program` exports an owned
//! `TypedProgram` with lexical binding identities and per-node dictionary
//! expressions through `check::check_program`/`check_library_program`.
//! `elaborate` retains the older per-type-variable evidence inspection API.

pub mod annotation;
pub mod ast;
pub mod check;
pub mod constrain;
pub mod elaborate;
pub mod error;
pub mod exhaustiveness;
pub mod interface;
pub mod kind;
pub mod predicate;
pub mod prelude;
pub mod print;
pub mod program;
pub mod sig;
pub mod solve;
pub mod ty;
pub mod unify;
pub mod var;

pub mod host;
pub mod modules;
