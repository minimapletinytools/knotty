//! A source-backed projection of one checked definition. Expression ids and parameter
//! ports belong to their lexical box and are meaningful only in this snapshot.
mod fallback;
mod position;
mod recursion;
use position::{Patch, Placement};

use crate::project::ProjectSnapshot;
use knot_canonical::ast::{CAnnotation, CDecl, CPattern, Ref};
use knot_checker::{
    ast::{NodeId, TDoStmt, TExpr, TPattern, TypedExpr, TypedPattern},
    exhaustiveness::WarningKind,
    print::{print_scheme, Printer},
    program::{BindingId, BindingInfo, EvidenceSite, ResolvedReference, TypedProgram},
    solve::scheme_key,
    ty::Structure,
    var::{Substitution, TypeVarId},
};
use knot_syntax::{ParseState, Span};
use serde::{Deserialize, Serialize};

/// Each layer belongs to one declaration or annotated expression. Keep the
/// checked keys in order, including repeatable attachments such as @whip.
#[derive(Serialize)]
struct AnnotationLayer {
    owner: Span,
    entries: Vec<AnnotationEntry>,
}

#[derive(Serialize)]
struct AnnotationEntry {
    key: String,
    value: String,
    span: Span,
}

impl AnnotationLayer {
    fn from_checked(owner: Span, annotations: &[CAnnotation], source: &str) -> Option<Self> {
        if annotations.is_empty() {
            return None;
        }
        let entries = annotations
            .iter()
            .map(|a| {
                let span = a.value.span;
                AnnotationEntry {
                    key: a.key.clone(),
                    value: source[span.start as usize..span.end as usize].into(),
                    span,
                }
            })
            .collect();
        Some(Self { owner, entries })
    }
}

#[derive(Serialize)]
pub struct Graph {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    recursive: bool,
    pub snapshot: String,
    module: String,
    definition: String,
    ty: String,
    source: String,
    source_start: u32,
    callable: Option<Callable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    definition_port: Option<usize>,
    scope: ScopeBox,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    annotations: Vec<AnnotationLayer>,
    #[serde(flatten)]
    placement: Placement,
    #[serde(skip)]
    port_names: Vec<String>,
}

/// The outer side of each input accepts one argument. Its pattern introduces
/// zero or more bindings on the inner side. The body feeds the result boundary.
#[derive(Serialize)]
struct ScopeBox {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    unreachable: bool,
    inputs: Vec<BoundaryInput>,
    body: Box<Expr>,
}

#[derive(Serialize)]
struct BoundaryInput {
    pattern: String,
    ty: String,
    parameters: Vec<Parameter>,
    shape: PatternShape,
}

#[derive(Serialize, Default)]
struct PatternShape {
    label: String,
    bindings: Vec<String>,
    /// `as` names for the whole of this pattern, outermost last.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    /// A named data constructor, as opposed to tuple, record or list structure.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    constructor: bool,
    children: Vec<PatternShape>,
}

fn pattern_shape(p: &TypedPattern, source: &str) -> PatternShape {
    let leaf = |name: &String| PatternShape {
        bindings: vec![name.clone()],
        ..Default::default()
    };
    let shape = |label: String, children| PatternShape {
        label,
        children,
        ..Default::default()
    };
    let child = |p: &TypedPattern| pattern_shape(p, source);
    match &p.node {
        TPattern::Var(name) => leaf(name),
        TPattern::As(inner, name) => {
            let mut inner = child(inner);
            inner.aliases.push(name.clone());
            inner
        }
        TPattern::Ctor(r, parts) => PatternShape {
            constructor: true,
            ..shape(
                knot_checker::print::ref_name(r),
                parts.iter().map(child).collect(),
            )
        },
        TPattern::Tuple(parts) => shape(
            format!("({})", ",".repeat(parts.len() - 1)),
            parts.iter().map(child).collect(),
        ),
        TPattern::Record(fields) => shape(
            "{ }".into(),
            fields.iter().map(|(name, _)| leaf(name)).collect(),
        ),
        TPattern::Nil => shape("[]".into(), vec![]),
        TPattern::Cons(head, tail) => shape(":".into(), vec![child(head), child(tail)]),
        _ => shape(
            source[p.span.start as usize..p.span.end as usize].into(),
            vec![],
        ),
    }
}

#[derive(Serialize)]
struct Parameter {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    recursive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    callable: Option<Callable>,
    port: usize,
    name: String,
    ty: String,
}

#[derive(Serialize)]
struct ScopeStep {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    named_function: bool,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    statement: Option<usize>,
    pattern: Option<BoundaryInput>,
    value: Expr,
}

#[derive(Serialize)]
struct Callable {
    name: String,
    signature: String,
    at_site: String,
    origin: String,
}

#[derive(Serialize)]
pub struct Expr {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    recursive: bool,
    id: NodeId,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    annotations: Vec<AnnotationLayer>,
    #[serde(flatten)]
    placement: Placement,
    kind: &'static str,
    label: String,
    ty: String,
    inputs: Vec<Expr>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    source_ports: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callable: Option<Callable>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_inputs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied_result: Option<String>,
    parameter: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callee_port: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    constructor_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_span: Option<Span>,
    #[serde(skip_serializing_if = "Option::is_none")]
    composition_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<ScopeBox>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    arms: Vec<ScopeBox>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    incomplete: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    steps: Vec<ScopeStep>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    visible: Vec<usize>,
    /// A prefix operator written in the source, such as the minus in `-n`.
    #[serde(skip_serializing_if = "Option::is_none")]
    prefix: Option<&'static str>,
    span: Span,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Position {
        target: Option<NodeId>,
        x: f64,
        y: f64,
    },
    Number {
        target: NodeId,
        value: String,
    },
    Connect {
        target: NodeId,
        parameter: usize,
    },
}

fn number(text: &str) -> bool {
    let mut parser = ParseState::new(text);
    parser.number_literal().is_ok() && parser.is_eof()
}

fn backticked_name(gap: &str) -> Option<&str> {
    let mut cursor = ParseState::new(gap);
    cursor.skip_trivia().ok()?;
    let rest = gap[cursor.pos.offset as usize..].strip_prefix('`')?;
    Some(rest.split_once('`')?.0)
}

struct Projection<'a> {
    recursion: recursion::Recursion,
    program: &'a TypedProgram,
    types: Substitution,
    printer: Printer,
    source: &'a str,
    snapshot: &'a ProjectSnapshot,
    active: Vec<(BindingId, usize)>,
    port_names: Vec<String>,
}

impl Projection<'_> {
    fn bind_pattern(&mut self, p: &TypedPattern) -> BoundaryInput {
        let mut parameters = Vec::new();
        for id in &self.program.pattern_bindings[&p.id] {
            let binding = &self.program.bindings[id];
            let port = self.port_names.len();
            self.port_names.push(binding.name.clone());
            self.active.push((*id, port));
            parameters.push(Parameter {
                recursive: self.recursion.bindings.contains(id),
                callable: self.local_callable(binding, binding.scheme.ty),
                port,
                name: binding.name.clone(),
                ty: self.printer.ty(&mut self.types, binding.scheme.ty),
            });
        }
        BoundaryInput {
            pattern: self.source[p.span.start as usize..p.span.end as usize].into(),
            ty: self.printer.ty(&mut self.types, p.ty),
            parameters,
            shape: pattern_shape(p, self.source),
        }
    }

    fn scope(&mut self, patterns: &[TypedPattern], body: &TypedExpr) -> ScopeBox {
        let outer = self.active.len();
        let inputs = patterns.iter().map(|p| self.bind_pattern(p)).collect();
        let body = Box::new(self.expr(body));
        self.active.truncate(outer);
        ScopeBox {
            inputs,
            body,
            unreachable: false,
        }
    }

    fn let_group(
        &mut self,
        bindings: &[(TypedPattern, TypedExpr)],
        statement: Option<usize>,
    ) -> Vec<ScopeStep> {
        // Every binder is in scope for every right-hand side in the group.
        let members: Vec<_> = bindings
            .iter()
            .map(|(p, body)| (self.program.pattern_bindings[&p.id].as_slice(), body))
            .collect();
        self.recursion.group(self.program, &members);
        let patterns: Vec<_> = bindings.iter().map(|(p, _)| self.bind_pattern(p)).collect();
        patterns
            .into_iter()
            .zip(bindings)
            .map(|(pattern, (p, rhs))| {
                let mut value = self.expr(rhs);
                // A sugared lambda or declaration annotation spans the binder too.
                value.placement.before_binding = rhs.span.start <= p.span.start;
                ScopeStep {
                    named_function: value.scope.is_some()
                        && value.span.start == p.span.start
                        && matches!(&p.node, TPattern::Var(_)),
                    kind: "let",
                    statement,
                    pattern: Some(pattern),
                    value,
                }
            })
            .collect()
    }

    fn binding_port(&self, site: NodeId) -> Option<usize> {
        let ResolvedReference::Binding(binding) =
            self.program.references.get(&EvidenceSite::Expr(site))?
        else {
            return None;
        };
        self.active
            .iter()
            .find(|(id, _)| id == binding)
            .map(|(_, port)| *port)
    }

    fn application_types(&mut self, mut ty: TypeVarId) -> (Vec<String>, TypeVarId) {
        let mut inputs = Vec::new();
        while let Some(Structure::Fn(input, output)) = self.types.head_normal(ty) {
            inputs.push(self.printer.ty(&mut self.types, input));
            ty = output;
        }
        (inputs, ty)
    }

    fn local_callable(&mut self, binding: &BindingInfo, ty: TypeVarId) -> Option<Callable> {
        if !matches!(self.types.head_normal(ty), Some(Structure::Fn(..))) {
            return None;
        }
        let file = self.snapshot.entry_path().file_name()?.to_string_lossy();
        Some(Callable {
            name: binding.name.clone(),
            signature: print_scheme(&mut self.types, &binding.scheme),
            at_site: self.printer.ty(&mut self.types, ty),
            origin: format!("Local binding · {file}"),
        })
    }

    fn callable(&mut self, r: &Ref, site: NodeId, ty: TypeVarId) -> Option<Callable> {
        if !matches!(self.types.head_normal(ty), Some(Structure::Fn(..))) {
            return None;
        }
        if let Ref::Local(_) = r {
            let ResolvedReference::Binding(id) =
                self.program.references.get(&EvidenceSite::Expr(site))?
            else {
                return None;
            };
            return self.local_callable(&self.program.bindings[id], ty);
        }
        let global = self.program.qualify(r)?;
        let scheme = self.program.schemes.get(&scheme_key(r)?)?;
        let mut origin = self
            .snapshot
            .graph
            .modules
            .iter()
            .find(|m| m.program.canonical.name == global.module)
            .and_then(|m| m.path.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{}.knot", global.module.join("/")));
        if let Ref::Builtin(name) = r {
            if let Some(ResolvedReference::Method { interface, .. }) =
                self.program.references.get(&EvidenceSite::Expr(site))
            {
                origin.push_str(&format!(" · {interface}"));
            }
            if name
                .bytes()
                .all(knot_syntax::lex::operator::is_operator_char)
            {
                origin.push_str(" · Built-in operator");
            }
        }
        Some(Callable {
            name: global.name,
            signature: print_scheme(&mut self.types, scheme),
            at_site: self.printer.ty(&mut self.types, ty),
            origin,
        })
    }

    fn call_labels(&self, r: &Ref) -> Vec<String> {
        let Some(global) = self.program.qualify(r) else {
            return vec![];
        };
        let Some(module) = self
            .snapshot
            .graph
            .modules
            .iter()
            .find(|m| m.program.canonical.name == global.module)
        else {
            return vec![];
        };
        module
            .program
            .canonical
            .decls
            .iter()
            .find_map(|d| match &d.node {
                CDecl::Fn(f) if f.name == global.name => Some(
                    f.params
                        .iter()
                        .map(|p| match &p.node {
                            CPattern::Var(name) => name.clone(),
                            _ => module.source[p.span.start as usize..p.span.end as usize]
                                .to_string(),
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn has_warning(&self, span: Span, kind: WarningKind) -> bool {
        self.program
            .warnings
            .iter()
            .any(|w| w.span == span && w.kind == kind)
    }

    fn expr(&mut self, e: &TypedExpr) -> Expr {
        if let TExpr::Annotated(annotations, target) = &e.node {
            let mut expr = self.expr(target);
            expr.placement.span = e.span;
            expr.placement.read(annotations);
            if let Some(layer) = AnnotationLayer::from_checked(e.span, annotations, self.source) {
                expr.annotations.insert(0, layer);
            }
            return expr;
        }
        let source = &self.source[e.span.start as usize..e.span.end as usize];
        let mut inputs = Vec::new();
        let mut callable = None;
        let mut token_span = None;
        let mut recursive = self.recursion.uses.contains(&e.id);
        let mut constructor = matches!(&e.node, TExpr::Ctor(_));
        let mut parameter = None;
        let mut callee_port = None;
        let mut scope = None;
        let mut arms = Vec::new();
        let mut incomplete = false;
        let mut steps = Vec::new();
        let mut labels = Vec::new();
        let mut visible = Vec::new();
        let (kind, label) = match &e.node {
            TExpr::Lambda(patterns, body) => {
                scope = Some(self.scope(patterns, body));
                ("box", "λ")
            }
            TExpr::If(condition, yes, no) => {
                inputs.push(self.expr(condition));
                arms.extend([self.scope(&[], yes), self.scope(&[], no)]);
                ("if", "if")
            }
            TExpr::Case(scrutinee, alternatives) => {
                inputs.push(self.expr(scrutinee));
                incomplete = self.has_warning(e.span, WarningKind::NonExhaustiveMatch);
                for (pattern, body) in alternatives {
                    let mut arm = self.scope(std::slice::from_ref(pattern), body);
                    arm.unreachable =
                        self.has_warning(pattern.span, WarningKind::RedundantMatchArm);
                    arms.push(arm);
                }
                ("case", "case")
            }
            TExpr::Let(bindings, result) => {
                let outer = self.active.len();
                steps = self.let_group(bindings, None);
                inputs.push(self.expr(result));
                self.active.truncate(outer);
                ("let", "let")
            }
            TExpr::Do(statements, result) => {
                let outer = self.active.len();
                for (index, stmt) in statements.iter().enumerate() {
                    match stmt {
                        TDoStmt::Bind(p, rhs, _) => {
                            let value = self.expr(rhs);
                            steps.push(ScopeStep {
                                named_function: false,
                                kind: "bind",
                                statement: Some(index),
                                pattern: Some(self.bind_pattern(p)),
                                value,
                            });
                        }
                        TDoStmt::Expr(rhs, _) => steps.push(ScopeStep {
                            named_function: false,
                            kind: "discard",
                            statement: Some(index),
                            pattern: None,
                            value: self.expr(rhs),
                        }),
                        TDoStmt::Let(bindings) => {
                            steps.extend(self.let_group(bindings, Some(index)))
                        }
                    }
                }
                inputs.push(self.expr(result));
                self.active.truncate(outer);
                ("do", "do")
            }
            TExpr::App(_, _) => {
                let mut head = e;
                let mut args = Vec::new();
                while let TExpr::App(callee, arg) = &head.node {
                    args.push(arg.as_ref());
                    head = callee;
                }
                recursive = self.recursion.uses.contains(&head.id);
                constructor = matches!(head.node, TExpr::Ctor(_));
                if let TExpr::Var(r) | TExpr::Ctor(r) = &head.node {
                    callable = self.callable(r, head.id, head.ty);
                }
                if let TExpr::Var(r) = &head.node {
                    labels = self.call_labels(r);
                }
                let call = if self.binding_port(head.id).is_none()
                    && matches!(
                        head.node,
                        TExpr::Var(Ref::Imported { .. } | Ref::Builtin(_) | Ref::TopLevel(_))
                            | TExpr::Ctor(_)
                    ) {
                    (
                        "call",
                        &self.source[head.span.start as usize..head.span.end as usize],
                    )
                } else {
                    inputs.push(self.expr(head));
                    ("callsite", "Call")
                };
                inputs.extend(args.into_iter().rev().map(|arg| self.expr(arg)));
                call
            }
            TExpr::Negate(value, op_ty) => {
                inputs.push(self.expr(value));
                callable = self.callable(&Ref::Builtin("negate".into()), e.id, *op_ty);
                ("call", "negate")
            }
            TExpr::Unit => ("value", "()"),
            TExpr::BinOp(op, left, right, op_ty) => {
                callable = self.callable(op, e.id, *op_ty);
                inputs.extend([self.expr(left), self.expr(right)]);
                let name = match op {
                    Ref::Imported { name, .. }
                    | Ref::Builtin(name)
                    | Ref::Local(name)
                    | Ref::TopLevel(name)
                    | Ref::Unresolved(name) => name,
                };
                let kind = match op {
                    Ref::Builtin(n) if n == "|>" => "pipe",
                    Ref::Builtin(n) if n == "|>>" => "composition",
                    Ref::Local(_) | Ref::TopLevel(_) => {
                        callee_port = self.binding_port(e.id);
                        "operator"
                    }
                    _ => "operator",
                };
                // Resolution discards the written qualifier, including import aliases.
                let label = self
                    .source
                    .get(left.span.end as usize..right.span.start as usize)
                    .and_then(backticked_name)
                    .unwrap_or(name);
                (kind, label)
            }
            TExpr::Record(fields) | TExpr::RecordUpdate(_, fields) => {
                let (kind, label) = if let TExpr::RecordUpdate(base, _) = &e.node {
                    inputs.push(self.expr(base));
                    labels.push("record".into());
                    ("record-update", "Update record")
                } else {
                    ("record", "Record")
                };
                for (field, value) in fields {
                    labels.push(field.clone());
                    inputs.push(self.expr(value));
                }
                (kind, label)
            }
            TExpr::FieldAccess(base, field) => {
                inputs.push(self.expr(base));
                ("field-access", field.as_str())
            }
            TExpr::List(items) | TExpr::Tuple(items) => {
                inputs.extend(items.iter().map(|arg| self.expr(arg)));
                if matches!(e.node, TExpr::List(_)) {
                    ("list", "List")
                } else {
                    ("tuple", "Tuple")
                }
            }
            TExpr::Var(Ref::Local(name) | Ref::TopLevel(name))
                if self.binding_port(e.id).is_some() =>
            {
                token_span = crate::lint::grouped_name_span(source, name, e.span.start);
                if source == name || token_span.is_some() {
                    parameter = self.binding_port(e.id);
                    let mut names = std::collections::BTreeSet::new();
                    visible.extend(self.active.iter().rev().filter_map(|(id, port)| {
                        names
                            .insert(&self.program.bindings[id].name)
                            .then_some(*port)
                    }));
                }
                (
                    if parameter.is_some() {
                        "reference"
                    } else {
                        "source"
                    },
                    if parameter.is_some() {
                        name.as_str()
                    } else {
                        source
                    },
                )
            }
            TExpr::Var(Ref::Local(_)) => ("source", source),
            TExpr::Var(_) | TExpr::Ctor(_) | TExpr::StringLit(_) => ("value", source),
            TExpr::IntLit(_) | TExpr::FloatLit(_) => {
                token_span = crate::lint::grouped_token_span(source, e.span.start, |p| {
                    Some(p.number_literal().ok()?.span)
                });
                if let Some(span) = token_span {
                    (
                        "number",
                        &self.source[span.start as usize..span.end as usize],
                    )
                } else if number(source) {
                    ("number", source)
                } else {
                    ("source", source)
                }
            }
            _ => ("source", source),
        };
        if let TExpr::Var(r) | TExpr::Ctor(r) = &e.node {
            callable = self.callable(r, e.id, e.ty);
        }
        let ty = self.printer.ty(&mut self.types, e.ty);
        let composition_result = match (kind, self.types.head_normal(e.ty)) {
            ("composition", Some(Structure::Fn(_, result))) => {
                Some(self.printer.ty(&mut self.types, result))
            }
            _ => None,
        };
        let label = match (&e.node, self.types.head_normal(e.ty)) {
            (TExpr::List(_), Some(Structure::App(_, args))) if args.len() == 1 => {
                format!("[{}]", self.printer.ty(&mut self.types, args[0]))
            }
            (TExpr::Tuple(_), _) => ty.clone(),
            _ => label.to_owned(),
        };
        let (open_inputs, result) = self.application_types(e.ty);
        Expr {
            recursive,
            callable,
            applied_result: (!open_inputs.is_empty())
                .then(|| self.printer.ty(&mut self.types, result)),
            open_inputs,
            composition_result,
            token_span,
            constructor_type: constructor.then(|| self.printer.ty(&mut self.types, result)),
            callee_port,
            source_ports: if kind == "source" {
                self.fallback_ports(e)
            } else {
                vec![]
            },
            id: e.id,
            annotations: vec![],
            placement: Placement::new(e.span),
            kind,
            label,
            ty,
            inputs,
            parameter,
            prefix: matches!(e.node, TExpr::Negate(..)).then_some("-"),
            span: e.span,
            scope,
            arms,
            incomplete,
            steps,
            labels,
            visible,
        }
    }
}

pub fn project(snapshot: &ProjectSnapshot, name: &str) -> Result<Graph, String> {
    let program = &snapshot.graph.entry().program;
    let decl = program
        .canonical
        .decls
        .iter()
        .find(|d| matches!(&d.node, CDecl::Fn(f) if f.name == name))
        .ok_or_else(|| format!("Unknown definition {name}."))?;
    let CDecl::Fn(function) = &decl.node else {
        unreachable!()
    };
    let original = snapshot
        .editable_source(snapshot.entry_path(), decl.span)
        .ok_or("This definition has no editable source.")?;
    let definition = program
        .definition(name)
        .ok_or("Missing checked definition.")?;
    let (patterns, body) = if function.params.is_empty() {
        (&[][..], &definition.body)
    } else if let TExpr::Lambda(patterns, inner) = &definition.body.node {
        (patterns.as_slice(), inner.as_ref())
    } else {
        return Err("Missing checked parameters.".into());
    };
    let mut recursion = recursion::Recursion::default();
    let members: Vec<_> = program
        .definitions
        .iter()
        .map(|d| (std::slice::from_ref(&d.binding), &d.body))
        .collect();
    recursion.group(program, &members);
    let mut view = Projection {
        recursion,
        program,
        types: program.substitution.clone(),
        printer: Printer::new(),
        source: &snapshot.sources()[snapshot.entry_path()],
        snapshot,
        active: vec![],
        port_names: vec![],
    };
    let definition_port = matches!(
        view.types.head_normal(definition.body.ty),
        Some(Structure::Fn(..))
    )
    .then(|| {
        let port = view.port_names.len();
        view.port_names.push(name.into());
        view.active.push((definition.binding, port));
        port
    });
    let mut placement = Placement::new(decl.span);
    placement.read(&function.annotations);
    Ok(Graph {
        recursive: view.recursion.bindings.contains(&definition.binding),
        annotations: AnnotationLayer::from_checked(decl.span, &function.annotations, view.source)
            .into_iter()
            .collect(),
        placement,
        snapshot: snapshot.id().into(),
        module: program.canonical.name.join("."),
        definition: name.into(),
        ty: view.printer.ty(&mut view.types, definition.body.ty),
        source: original.into(),
        source_start: decl.span.start,
        callable: view.callable(
            &Ref::TopLevel(name.into()),
            definition.body.id,
            definition.body.ty,
        ),
        definition_port,
        scope: view.scope(patterns, body),
        port_names: view.port_names,
    })
}

impl Expr {
    fn find(&self, id: NodeId) -> Option<&Self> {
        if self.id == id {
            Some(self)
        } else {
            self.scope
                .as_ref()
                .and_then(|s| s.body.find(id))
                .or_else(|| self.arms.iter().find_map(|arm| arm.body.find(id)))
                .or_else(|| self.steps.iter().find_map(|step| step.value.find(id)))
                .or_else(|| self.inputs.iter().find_map(|child| child.find(id)))
        }
    }
}

/// Compose token replacements and annotation insertions against one checked snapshot.
pub fn candidate(
    snapshot: &ProjectSnapshot,
    name: &str,
    changes: &[Change],
) -> Result<String, String> {
    let graph = project(snapshot, name)?;
    let source = &snapshot.sources()[snapshot.entry_path()];
    let mut moved = std::collections::BTreeSet::new();
    let mut patches = Vec::new();
    for change in changes {
        let (id, expected, text) = match change {
            Change::Position { target, x, y } => {
                if !moved.insert(target) {
                    return Err("Two positions target the same node.".into());
                }
                let (placement, atom) = if let Some(id) = target {
                    let e = graph.scope.body.find(*id).ok_or("Unknown graph node.")?;
                    (
                        &e.placement,
                        matches!(e.kind, "number" | "value" | "reference"),
                    )
                } else {
                    (&graph.placement, false)
                };
                placement.edit([*x, *y], target.is_none(), atom, source, &mut patches)?;
                continue;
            }
            Change::Number { target, value } => {
                if !number(value) {
                    return Err("Enter one unsigned Knot numeric literal.".into());
                }
                (*target, "number", value.as_str())
            }
            Change::Connect { target, parameter } => {
                let p = graph
                    .port_names
                    .get(*parameter)
                    .ok_or("Unknown parameter port.")?;
                (*target, "reference", p.as_str())
            }
        };
        let site = graph
            .scope
            .body
            .find(id)
            .filter(|e| e.kind == expected)
            .ok_or("This socket does not accept that edit.")?;
        if let Change::Connect { parameter, .. } = change {
            if !site.visible.contains(parameter) {
                return Err("This parameter is outside the socket’s scope or shadowed.".into());
            }
        }
        patches.push(Patch::replace(
            site.token_span.unwrap_or(site.span),
            text.into(),
        ));
    }
    Patch::apply(patches, source.clone())
}
