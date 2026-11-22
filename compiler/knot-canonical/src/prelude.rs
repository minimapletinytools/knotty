//! The built-in environment, read from the library modules in `lib/`:
//! `Prelude.knot` (in scope everywhere, unqualified), and `Map.knot` and
//! `String.knot` (used qualified, `Map.get` and `String.length`, as Haskell
//! uses `Data.Map` and `Data.Text`). They are ordinary
//! Knot source, parsed with `knot_syntax::parse_library`, and are the
//! specification of every builtin type, constructor, interface (methods,
//! superclasses, defaults), instance, value, and fixity.
//!
//! This crate only needs the *names* (to resolve references); `knot-checker`
//! reads the same signatures for their types, and type-checks the modules'
//! bodies (`knot_checker::check::check_library`).
//!
//! The files are compiled into the binary and parsed once, lazily. A
//! malformed library is a bug in this crate and panics (covered by this
//! module's tests).

use std::sync::OnceLock;

use knot_syntax::ast::decl::{Decl, ExposedItem, Exposing, Fixity, Module};
use knot_syntax::ast::ty::{Constraint, Type, TypeSignature};

use crate::ast::Ref;

/// Each library module's name and source.
pub const LIBRARY: &[(&str, &str)] = &[
    ("Prelude", include_str!("../lib/Prelude.knot")),
    ("Map", include_str!("../lib/Map.knot")),
    ("String", include_str!("../lib/String.knot")),
];

#[derive(Debug, Clone)]
pub struct BuiltinType {
    /// Empty for a Prelude type; `["Map"]` for `Map.Map`.
    pub module: Vec<String>,
    pub name: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuiltinCtor {
    pub name: String,
    pub type_name: String,
    pub arity: usize,
    pub sig: TypeSignature,
}

#[derive(Debug, Clone)]
pub struct BuiltinInterface {
    pub name: String,
    /// The interface's own type variables (`a` in `Eq a`, `s t` in `Constriction s t`).
    pub vars: Vec<String>,
    pub superclasses: Vec<String>,
    pub methods: Vec<(String, TypeSignature)>,
    /// The methods that have a default body, so an instance may omit them.
    pub defaults: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BuiltinInstance {
    pub interface: String,
    pub context: Vec<Constraint>,
    /// A type whose head is a builtin type's name, qualified (`Map.Map`)
    /// for a type from a qualified module — see `builtin_type_ref`.
    pub targets: Vec<Type>,
}

#[derive(Debug, Clone)]
pub struct BuiltinValue {
    /// Empty for an unqualified prelude value; `["Map"]` for `Map.get`.
    pub module: Vec<String>,
    pub name: String,
    pub sig: TypeSignature,
}

#[derive(Debug, Clone, Default)]
pub struct Prelude {
    pub types: Vec<BuiltinType>,
    pub ctors: Vec<BuiltinCtor>,
    pub interfaces: Vec<BuiltinInterface>,
    pub instances: Vec<BuiltinInstance>,
    pub values: Vec<BuiltinValue>,
    /// Operator (or backticked function) name -> its declared fixity.
    pub fixities: Vec<(String, Fixity)>,
}

impl Prelude {
    pub fn interface(&self, name: &str) -> Option<&BuiltinInterface> {
        self.interfaces.iter().find(|i| i.name == name)
    }

    /// The interface that declares `method`, if it is a method at all.
    pub fn method_owner(&self, method: &str) -> Option<&BuiltinInterface> {
        self.interfaces
            .iter()
            .find(|i| i.methods.iter().any(|(m, _)| m == method))
    }
}

pub fn prelude() -> &'static Prelude {
    static PRELUDE: OnceLock<Prelude> = OnceLock::new();
    PRELUDE.get_or_init(|| {
        let mut prelude = Prelude::default();
        for (name, module) in library_modules() {
            add_module(&mut prelude, name, module).unwrap_or_else(|e| panic!("{name}.knot: {e}"));
        }
        prelude
    })
}

/// Every library module, parsed.
pub fn library_modules() -> &'static [(&'static str, Module)] {
    static MODULES: OnceLock<Vec<(&'static str, Module)>> = OnceLock::new();
    MODULES.get_or_init(|| {
        LIBRARY
            .iter()
            .map(|(name, source)| {
                let module = knot_syntax::parse_library(source)
                    .unwrap_or_else(|e| panic!("{name}.knot: {}", e.render(source)));
                (*name, module)
            })
            .collect()
    })
}

/// How a builtin type's name (as `BuiltinInstance::target` and signatures
/// spell it) becomes a reference: `Map.Map` is imported from `Map`,
/// anything unqualified is a Prelude builtin.
pub fn builtin_type_ref(name: &str) -> Ref {
    match name.rsplit_once('.') {
        Some((module, name)) => Ref::Imported {
            module: module.split('.').map(str::to_string).collect(),
            name: name.to_string(),
        },
        None => Ref::Builtin(name.to_string()),
    }
}

fn exposes(exposing: &Exposing, name: &str, with_ctors: bool) -> bool {
    match exposing {
        Exposing::All => true,
        Exposing::Some(items) => items.iter().any(|item| match item {
            ExposedItem::Value(n) | ExposedItem::TypeOnly(n) => n == name && !with_ctors,
            ExposedItem::TypeWithVariants(n) => n == name,
        }),
    }
}

/// Renames the module's own types to their qualified spelling (`Map` ->
/// `Map.Map`) inside a qualified module's signatures.
fn qualify(ty: &Type, module: &str, own: &[String]) -> Type {
    let q = |t: &Type| qualify(t, module, own);
    match ty {
        Type::Named(name, args) => {
            let name = if own.contains(name) {
                format!("{module}.{name}")
            } else {
                name.clone()
            };
            Type::Named(name, args.iter().map(q).collect())
        }
        Type::VarApp(v, args) => Type::VarApp(v.clone(), args.iter().map(q).collect()),
        Type::Fn(a, b) => Type::Fn(Box::new(q(a)), Box::new(q(b))),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(q).collect()),
        Type::Record(fields, ext) => Type::Record(
            fields.iter().map(|(n, t)| (n.clone(), q(t))).collect(),
            ext.clone(),
        ),
        Type::Var(_) | Type::Unit => ty.clone(),
    }
}

fn add_module(prelude: &mut Prelude, name: &str, module: &Module) -> Result<(), String> {
    let home: Vec<String> = if name == "Prelude" {
        Vec::new()
    } else {
        vec![name.to_string()]
    };
    let own_types: Vec<String> = module
        .decls
        .iter()
        .filter_map(|d| match &d.node {
            Decl::TypeDecl(n, ..) | Decl::TypeAlias(n, ..) => Some(n.clone()),
            _ => None,
        })
        .collect();
    let fix = |ty: &Type| {
        if home.is_empty() {
            ty.clone()
        } else {
            qualify(ty, name, &own_types)
        }
    };
    let fix_sig = |sig: &TypeSignature| TypeSignature {
        constraints: sig.constraints.clone(),
        ty: fix(&sig.ty),
    };
    let exposed = |n: &str, with_ctors: bool| exposes(&module.exposing, n, with_ctors);

    for decl in &module.decls {
        match &decl.node {
            Decl::TypeDecl(type_name, params, variants, deriving) => {
                let result = fix(&Type::Named(
                    type_name.clone(),
                    params.iter().map(|p| Type::Var(p.clone())).collect(),
                ));
                if exposed(type_name, false) || exposed(type_name, true) {
                    prelude.types.push(BuiltinType {
                        module: home.clone(),
                        name: type_name.clone(),
                        params: params.clone(),
                    });
                }
                if home.is_empty() && exposed(type_name, true) {
                    for (ctor, fields) in variants {
                        let ty = fields.iter().rev().fold(result.clone(), |acc, f| {
                            Type::Fn(Box::new(f.clone()), Box::new(acc))
                        });
                        prelude.ctors.push(BuiltinCtor {
                            name: ctor.clone(),
                            type_name: type_name.clone(),
                            arity: fields.len(),
                            sig: TypeSignature {
                                constraints: Vec::new(),
                                ty,
                            },
                        });
                    }
                }
                // A derived instance needs the interface on every parameter,
                // which is exactly right for the library's own ADTs.
                for interface in deriving {
                    prelude.instances.push(BuiltinInstance {
                        interface: interface.clone(),
                        context: params
                            .iter()
                            .map(|p| Constraint {
                                interface: interface.clone(),
                                type_vars: vec![p.clone()],
                            })
                            .collect(),
                        targets: vec![result.clone()],
                    });
                }
            }
            Decl::TypeAlias(alias, ..) => {
                return Err(format!(
                    "type alias `{alias}` isn't supported in a library module"
                ))
            }
            Decl::Interface(i) => {
                if !home.is_empty() {
                    return Err(format!(
                        "interface `{}` must be declared in the Prelude",
                        i.name
                    ));
                }
                prelude.interfaces.push(BuiltinInterface {
                    name: i.name.clone(),
                    vars: i.vars.clone(),
                    superclasses: i.superclasses.iter().map(|c| c.interface.clone()).collect(),
                    methods: i
                        .methods
                        .iter()
                        .map(|(m, sig)| (m.clone(), sig.node.clone()))
                        .collect(),
                    defaults: i.defaults.iter().map(|f| f.name.clone()).collect(),
                });
            }
            Decl::Instance(inst) => prelude.instances.push(BuiltinInstance {
                interface: inst.interface.clone(),
                context: inst.constraints.clone(),
                targets: inst.targets.iter().map(fix).collect(),
            }),
            Decl::Fn(f) => {
                let sig = f
                    .signature
                    .as_ref()
                    .ok_or_else(|| format!("`{}` needs a type signature", f.name))?;
                if exposed(&f.name, false) {
                    prelude.values.push(BuiltinValue {
                        module: home.clone(),
                        name: f.name.clone(),
                        sig: fix_sig(&sig.node),
                    });
                }
            }
            Decl::Primitive(value, sig) => {
                if exposed(value, false) {
                    prelude.values.push(BuiltinValue {
                        module: home.clone(),
                        name: value.clone(),
                        sig: fix_sig(&sig.node),
                    });
                }
            }
            Decl::Fixity(f) => {
                for op in &f.operators {
                    prelude.fixities.push((op.node.clone(), f.fixity));
                }
            }
        }
    }
    Ok(())
}

pub fn is_builtin_type(name: &str) -> bool {
    prelude()
        .types
        .iter()
        .any(|t| t.module.is_empty() && t.name == name)
}

pub fn is_builtin_interface(name: &str) -> bool {
    prelude().interface(name).is_some()
}

/// An unqualified builtin value or interface method, operators included —
/// what a bare reference (`map`, `+`) can resolve to.
pub fn is_builtin_value(name: &str) -> bool {
    let p = prelude();
    p.values
        .iter()
        .any(|v| v.module.is_empty() && v.name == name)
        || p.method_owner(name).is_some()
}

/// A builtin operator's declared fixity, if it has one.
pub fn builtin_fixity(name: &str) -> Option<Fixity> {
    prelude()
        .fixities
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, f)| *f)
}

/// `(arity, owning type)` for a builtin data constructor.
pub fn builtin_constructor(name: &str) -> Option<(usize, &'static str)> {
    prelude()
        .ctors
        .iter()
        .find(|c| c.name == name)
        .map(|c| (c.arity, c.type_name.as_str()))
}

/// Every builtin data constructor as `(name, arity, owning type)`.
pub fn builtin_constructors() -> impl Iterator<Item = (&'static str, usize, &'static str)> {
    prelude()
        .ctors
        .iter()
        .map(|c| (c.name.as_str(), c.arity, c.type_name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prelude_parses() {
        let p = prelude();
        assert!(p.types.len() >= 10);
        assert!(p.interface("Traversable").is_some());
        assert_eq!(builtin_constructor("Just"), Some((1, "Maybe")));
        assert_eq!(builtin_constructor("Err"), Some((1, "Result")));
        assert!(is_builtin_value("compare"));
        assert!(is_builtin_value("traverse"));
        assert!(is_builtin_value("filter"));
        assert!(is_builtin_value("=="));
        assert!(is_builtin_value("|>"));
        assert!(!is_builtin_value("+++"));
    }

    #[test]
    fn every_builtin_operator_has_a_fixity() {
        use knot_syntax::ast::decl::Assoc;
        let p = prelude();
        let operators = p
            .values
            .iter()
            .map(|v| v.name.as_str())
            .chain(
                p.interfaces
                    .iter()
                    .flat_map(|i| i.methods.iter().map(|(m, _)| m.as_str())),
            )
            .filter(|n| {
                !n.starts_with(|c: char| {
                    c.is_ascii_uppercase() || knot_syntax::lex::ident::is_value_ident_start(c)
                })
            });
        for op in operators {
            assert!(builtin_fixity(op).is_some(), "{op} has no fixity");
        }
        let div = builtin_fixity("div").unwrap();
        assert_eq!((div.assoc, div.precedence), (Assoc::Left, 7));
        assert_eq!(builtin_fixity("==").unwrap().assoc, Assoc::None);
    }

    #[test]
    fn interfaces_record_their_variable_superclasses_and_defaults() {
        let integral = prelude().interface("Integral").unwrap();
        assert_eq!(integral.vars, ["a"]);
        assert_eq!(integral.superclasses, vec!["Real", "Enum"]);
        let ord = prelude().interface("Ord").unwrap();
        assert!(ord.defaults.contains(&"max".to_string()));
        assert!(!ord.defaults.contains(&"compare".to_string()));
        assert_eq!(prelude().interface("Traversable").unwrap().vars, ["t"]);
    }

    #[test]
    fn map_is_a_qualified_module() {
        let get = prelude().values.iter().find(|v| v.name == "get").unwrap();
        assert_eq!(get.module, vec!["Map".to_string()]);
        assert!(!is_builtin_value("get"));
        // `Map` the type is `Map.Map`, and its constructor stays private.
        assert!(!is_builtin_type("Map"));
        assert!(builtin_constructor("MkMap").is_none());
        let Type::Fn(_, rest) = &get.sig.ty else {
            panic!("get is a function")
        };
        let Type::Fn(map_ty, _) = rest.as_ref() else {
            panic!("get takes two arguments")
        };
        assert!(matches!(map_ty.as_ref(), Type::Named(n, _) if n == "Map.Map"));
    }

    #[test]
    fn deriving_in_the_prelude_makes_instances() {
        let derived = prelude()
            .instances
            .iter()
            .find(|i| {
                i.interface == "Ord" && matches!(&i.targets[0], Type::Named(n, _) if n == "Maybe")
            })
            .expect("Ord (Maybe a)");
        assert_eq!(derived.context.len(), 1);
    }
}
