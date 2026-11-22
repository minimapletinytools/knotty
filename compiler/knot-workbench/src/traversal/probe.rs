//! Small, checked queries against an immutable project. No runtime handles are
//! cast to guessed types, and queries never enable full execution tracing.
use super::*;
#[cfg(feature = "incremental")]
mod baseline;
mod dependencies;
mod live;
mod pool;
mod validation;
use crate::operation::{TypedValue, ValueType};
use knot_canonical::env::{CtorInfo, ModuleInterface, ModuleRegistry};
use knot_checker::{
    modules::CheckedInterface,
    ty::Structure,
    var::{Substitution, TypeVarId},
};
use knot_syntax::ast::decl::{Exposing, Fixity};
use live::{route, Live};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};
use twine::{ValueHandle, ValueView};

#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Literal(knot_core::Literal),
    Constructor(GlobalName, Vec<Data>),
    Tuple(Vec<Data>),
    Record(BTreeMap<String, Data>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Ty {
    Named(String, Vec<Ty>),
    Tuple(Vec<Ty>),
    Record(BTreeMap<String, Ty>),
    Function(Box<Ty>, Box<Ty>),
    Unit,
}
impl Ty {
    pub fn read(sub: &mut Substitution, id: TypeVarId) -> Result<Self, EditError> {
        Ok(match sub.head_normal(id) {
            Some(Structure::App(r, args)) => Self::Named(
                match &r {
                    Ref::Imported { module, name } if module == &["Prelude"] => name.clone(),
                    _ => knot_checker::print::ref_name(&r),
                },
                args.into_iter()
                    .map(|a| Self::read(sub, a))
                    .collect::<Result<_, _>>()?,
            ),
            Some(Structure::Tuple(ts)) => Self::Tuple(
                ts.into_iter()
                    .map(|t| Self::read(sub, t))
                    .collect::<Result<_, _>>()?,
            ),
            Some(Structure::Record(mut fs, mut extension)) => {
                while let Some(row) = extension {
                    let Some(Structure::Record(more, rest)) = sub.head_normal(row) else {
                        return unsupported("reverse traversal requires closed record types");
                    };
                    fs.extend(more);
                    extension = rest;
                }
                Self::Record(
                    fs.into_iter()
                        .map(|(n, t)| Ok((n, Self::read(sub, t)?)))
                        .collect::<Result<_, EditError>>()?,
                )
            }
            Some(Structure::Fn(a, b)) => {
                Self::Function(Box::new(Self::read(sub, a)?), Box::new(Self::read(sub, b)?))
            }
            Some(Structure::Unit) => Self::Unit,
            _ => return unsupported("reverse traversal requires a concrete type"),
        })
    }
    pub fn source(&self) -> String {
        match self {
            Self::Named(n, args) => format!(
                "({}{})",
                n,
                args.iter()
                    .map(|a| format!(" {}", a.source()))
                    .collect::<String>()
            ),
            Self::Tuple(ts) => format!(
                "({})",
                ts.iter().map(Self::source).collect::<Vec<_>>().join(", ")
            ),
            Self::Record(fs) => format!(
                "{{ {} }}",
                fs.iter()
                    .map(|(n, t)| format!("{n} : {}", t.source()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Function(a, b) => format!("({} -> {})", a.source(), b.source()),
            Self::Unit => "()".into(),
        }
    }
}
pub(super) fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
impl Data {
    pub(super) fn source(&self) -> Result<String, EditError> {
        use knot_core::Literal;
        Ok(match self {
            Self::Literal(Literal::Float(n)) if n.is_finite() => {
                let s = format!("{n:?}");
                format!("({s})")
            }
            Self::Literal(Literal::Int(n)) => format!("({n})"),
            Self::Literal(Literal::Integer(n)) => format!("({n})"),
            Self::Literal(Literal::String(s)) => quoted(s),
            Self::Literal(Literal::Unit) => "()".into(),
            Self::Constructor(n, args) => format!(
                "({}{} )",
                global_source(n),
                args.iter()
                    .map(|a| a.source().map(|s| format!(" {s}")))
                    .collect::<Result<String, _>>()?
            ),
            Self::Tuple(xs) => format!(
                "({})",
                xs.iter()
                    .map(Self::source)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            ),
            Self::Record(fs) => format!(
                "{{ {} }}",
                fs.iter()
                    .map(|(n, v)| Ok(format!("{n} = {}", v.source()?)))
                    .collect::<Result<Vec<_>, EditError>>()?
                    .join(", ")
            ),
            _ => return Err(EditError::NonFinite),
        })
    }
}
pub(super) fn global_source(n: &GlobalName) -> String {
    if n.module == ["Prelude"] && n.name == "[]" {
        return "[]".into();
    }
    let q = if n.module.is_empty() {
        n.name.clone()
    } else {
        format!("{}.{}", n.module.join("."), n.name)
    };
    if n.name
        .chars()
        .next()
        .is_some_and(|c| "+-*/<>=:&|^!%.".contains(c))
    {
        if n.module.is_empty() || n.module == ["Prelude"] {
            format!("({})", n.name)
        } else {
            format!("({q})")
        }
    } else {
        q
    }
}

pub(super) struct ProbeContext {
    route: std::sync::OnceLock<Arc<route::Image>>,
    #[cfg(feature = "incremental")]
    baseline: Mutex<Option<crate::operation::OperationSession>>,
    imports: knot_checker::check::PreparedImports,
    dependency_base: knot_core::link::LinkedProgram,
    base: knot_core::link::LinkedProgram,
    header: String,
    value_prefix: String,
    checked: Mutex<VecDeque<(String, Arc<CheckedProbe>)>>,
    monomorphic_globals: std::sync::OnceLock<BTreeMap<GlobalName, Ty>>,
    type_modules: Vec<Arc<knot_checker::program::TypedProgram>>,
}
struct CheckedProbe {
    program: Arc<knot_checker::program::TypedProgram>,
    // Exact metadata source permits a hit before parsing/name resolution.
    // Live operations already have an exact-source key, so leave this empty.
    source: String,
    frame: std::sync::OnceLock<Arc<super::call_frame::CallPlan>>,
}
pub(super) struct Probes {
    recording: Option<route::Recording>,
    pending_route: Option<Arc<route::Image>>,
    route_image: Option<Arc<route::Image>>,
    pub route_hits: usize,
    context: Arc<ProbeContext>,
    original_context: Arc<ProbeContext>,
    live: Option<Live>,
    validations: validation::Cache,
    validating: bool,
    references: BTreeMap<String, TypedValue>,
    pub declarations: String,
    pub usage: crate::unravel::EditUsage,
    pub(super) limits: EditLimits,
    pub count: usize,
    pub cache_hits: usize,
    materialized_values: usize,
    pub check_ms: f64,
    pub link_ms: f64,
    pub run_ms: f64,
    pub runtime_forks: usize,
    pub runtime_fork_work: u64,
}
struct Registry<'a>(&'a [Arc<CheckedInterface>]);
impl ModuleRegistry for Registry<'_> {
    fn exports(&self, m: &[String]) -> Option<ModuleInterface> {
        self.0
            .iter()
            .find(|i| i.name == m)
            .map(|i| i.exports.clone())
            .or_else(|| {
                knot_canonical::prelude::library_modules()
                    .iter()
                    .find(|(_, raw)| raw.name == m)
                    .and_then(|(_, raw)| knot_checker::modules::exported_names(raw).ok())
            })
    }
    fn aliases(&self, m: &[String]) -> HashMap<String, (Vec<String>, CType)> {
        self.0
            .iter()
            .find(|i| i.name == m)
            .map(|i| i.aliases.clone())
            .unwrap_or_default()
    }
    fn fixity(&self, m: &[String], n: &str) -> Option<Fixity> {
        self.0
            .iter()
            .find(|i| i.name == m)?
            .fixities
            .get(n)
            .copied()
    }
    fn constructor(&self, m: &[String], n: &str) -> Option<CtorInfo> {
        self.0
            .iter()
            .find(|i| i.name == m)?
            .constructors
            .get(n)
            .cloned()
    }
}
impl ProbeContext {
    pub fn new(
        snapshot: &ProjectSnapshot,
        libraries: &knot_core::Program,
    ) -> Result<Self, EditError> {
        Self::new_reusing(snapshot, libraries, None)
    }
    // Candidates belong to the same editor/library set as the baseline. Only an
    // exact checked dependency prefix can reuse its interfaces and linked code.
    fn new_reusing(
        snapshot: &ProjectSnapshot,
        libraries: &knot_core::Program,
        previous: Option<&Self>,
    ) -> Result<Self, EditError> {
        let type_modules: Vec<_> = snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.clone())
            .collect();
        let (entry, dependencies) = type_modules
            .split_last()
            .ok_or_else(|| EditError::Internal("query context has no entry module".into()))?;
        let previous = previous.filter(|old| {
            old.type_modules.len() == type_modules.len()
                && old
                    .type_modules
                    .iter()
                    .zip(dependencies)
                    .all(|(a, b)| Arc::ptr_eq(a, b))
        });
        let mut interfaces = vec![];
        for (index, m) in snapshot.graph.modules.iter().enumerate() {
            if let Some(old) = previous.filter(|_| index < dependencies.len()) {
                interfaces.push(old.imports.interfaces()[index].clone());
                continue;
            }
            // A module that exports everything already exposes the full probe scope.
            if m.exposes_all {
                interfaces.push(m.interface.clone());
                continue;
            }
            let mut raw =
                knot_syntax::parse(&m.source).map_err(|e| EditError::Parse(e.render(&m.source)))?;
            // Private bindings are visible only to internal, fully checked probes.
            raw.exposing = Exposing::All;
            interfaces.push(Arc::new(
                CheckedInterface::from_program(&m.program, &raw).map_err(EditError::Check)?,
            ));
        }
        let header = format!(
            "module TwineTraversalQuery exposing (..)\n{}\n",
            interfaces
                .iter()
                .map(|i| i.name.clone())
                .chain(
                    knot_canonical::prelude::library_modules()
                        .iter()
                        .map(|(_, m)| m.name.clone())
                )
                .map(|name| format!("import {}\n", name.join(".")))
                .collect::<String>()
        );
        if interfaces.iter().any(|i| i.name == ["TwineTraversalQuery"]) {
            return unsupported("reserved internal query module name collision");
        }
        let dependency_base = if let Some(old) = previous {
            old.dependency_base.clone()
        } else {
            let library =
                knot_core::link::LinkedProgram::new(libraries.clone(), &Default::default())
                    .map_err(EditError::Check)?;
            if dependencies.is_empty() {
                library
            } else {
                library
                    .extend(&dependencies.iter().map(Arc::as_ref).collect::<Vec<_>>())
                    .map_err(EditError::Check)?
            }
        };
        let base = dependency_base
            .extend(&[entry.as_ref()])
            .map_err(EditError::Check)?;
        let mut value_prefix = "twineRetainedValue".to_owned();
        while snapshot
            .sources()
            .values()
            .any(|source| source.contains(&value_prefix))
        {
            value_prefix.push('x');
        }
        Ok(Self {
            #[cfg(feature = "incremental")]
            baseline: Mutex::new(None),
            route: Default::default(),
            value_prefix,
            imports: knot_checker::check::PreparedImports::new(interfaces),
            dependency_base,
            base,
            header,
            checked: Mutex::new(VecDeque::new()),
            monomorphic_globals: Default::default(),
            type_modules,
        })
    }
}
impl Probes {
    pub(super) fn candidate_program(&self) -> Arc<knot_core::Program> {
        self.context.base.shared_program()
    }
    pub(super) fn validation_cache_hits(&self) -> usize {
        self.validations.hits
    }
    pub(super) fn monomorphic_global(&self, name: &GlobalName) -> Option<&Ty> {
        self.context
            .monomorphic_globals
            .get_or_init(|| {
                let mut globals = BTreeMap::new();
                for program in &self.context.type_modules {
                    for definition in &program.definitions {
                        let binding = &program.bindings[&definition.binding];
                        if binding.scheme.vars.is_empty() && binding.scheme.constraints.is_empty() {
                            if let (Some(name), Ok(ty)) =
                                (&binding.global, ValueType::read(program, binding.scheme.ty))
                            {
                                globals.insert(name.clone(), Ty::from_value(&ty));
                            }
                        }
                    }
                }
                globals
            })
            .get(name)
    }

    pub(super) fn call_plan(
        &mut self,
        source: &str,
        arity: usize,
    ) -> Result<Arc<super::call_frame::CallPlan>, EditError> {
        let (checked, _) = self.checked(source)?;
        if let Some(plan) = checked.frame.get() {
            return Ok(plan.clone());
        }
        let plan = Arc::new(super::call_frame::CallPlan::new(
            checked.program.clone(),
            arity,
        )?);
        let _ = checked.frame.set(plan.clone());
        Ok(plan)
    }
    pub fn from_context(context: Arc<ProbeContext>, limits: EditLimits) -> Self {
        Self {
            recording: None,
            pending_route: None,
            route_image: None,
            route_hits: 0,
            original_context: context.clone(),
            context,
            live: None,
            validations: Default::default(),
            validating: false,
            references: BTreeMap::new(),
            declarations: String::new(),
            usage: Default::default(),
            limits,
            count: 0,
            cache_hits: 0,
            materialized_values: 0,
            check_ms: 0.0,
            link_ms: 0.0,
            run_ms: 0.0,
            runtime_forks: 0,
            runtime_fork_work: 0,
        }
    }
    pub fn check(
        &mut self,
        body: &str,
    ) -> Result<Arc<knot_checker::program::TypedProgram>, EditError> {
        self.checked(body).map(|(p, _)| p.program.clone())
    }
    fn checked(
        &mut self,
        body: &str,
    ) -> Result<(Arc<CheckedProbe>, super::template::Template), EditError> {
        let started = Instant::now();
        if self
            .limits
            .runtime
            .cancellation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(twine::Outcome::Cancelled.into());
        }
        self.count += 1;
        if self.count > self.limits.traversal.queries {
            return unsupported("source traversal exceeded its checked-query budget");
        }
        let references = self.referenced(&format!("{}\n{body}", self.declarations));
        let stubs = references
            .iter()
            .map(|(name, value)| {
                format!(
                    "{name} :: {}\n{name} = undefined\n",
                    Ty::from_value(value.value_type()).source()
                )
            })
            .collect::<String>();
        let source = format!(
            "{}{stubs}{}\n{}\n",
            self.context.header, self.declarations, body
        );
        if source.len() > self.limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        {
            let mut cache = self.context.checked.lock().unwrap();
            if let Some(index) = cache.iter().position(|(_, entry)| entry.source == source) {
                let entry = cache.remove(index).unwrap();
                let checked = entry.1.clone();
                let template = super::template::Template {
                    source: entry.0.clone(),
                };
                cache.push_back(entry);
                self.cache_hits += 1;
                self.check_ms += started.elapsed().as_secs_f64() * 1000.;
                return Ok((checked, template));
            }
        }
        let raw = knot_syntax::parse(&source)
            .map_err(|e| EditError::Parse(format!("{}\n{source}", e.render(&source))))?;
        // Metadata does not execute the old argument stubs. Normalize only
        // resolved generated references, retaining their types and user syntax.
        let canonical = if references.is_empty() {
            None
        } else {
            Some(
                knot_canonical::canonicalize_module(
                    &raw,
                    Some(&Registry(self.context.imports.interfaces())),
                )
                .map_err(|e| EditError::Check(format!("{e:?}\n{source}")))?,
            )
        };
        let template = if let Some(key) = canonical
            .as_ref()
            .and_then(|module| pool::metadata_key(&source, module, &references))
        {
            super::template::Template { source: key }
        } else {
            super::template::Template::new(&source, &raw)
        };
        {
            let mut cache = self.context.checked.lock().unwrap();
            if let Some(index) = cache.iter().position(|(key, _)| key == &template.source) {
                let entry = cache.remove(index).unwrap();
                let checked = entry.1.clone();
                self.cache_hits += 1;
                cache.push_back(entry);
                self.check_ms += started.elapsed().as_secs_f64() * 1000.0;
                return Ok((checked, template));
            }
        }
        let canonical = if let Some(canonical) = canonical {
            canonical
        } else {
            knot_canonical::canonicalize_module(
                &raw,
                Some(&Registry(self.context.imports.interfaces())),
            )
            .map_err(|e| EditError::Check(format!("{e:?}\n{source}")))?
        };
        let checked = self
            .context
            .imports
            .check(&canonical, &Default::default())
            .map(|program| {
                Arc::new(CheckedProbe {
                    program: Arc::new(program),
                    source: source.clone(),
                    frame: Default::default(),
                })
            })
            .map_err(|e| EditError::Check(format!("{e:?}\n{source}")));
        if let Ok(program) = &checked {
            let mut cache = self.context.checked.lock().unwrap();
            cache.push_back((template.source.clone(), program.clone()));
            // Deep routes can have more than 64 distinct templates. Bound both
            // entry count and retained source size, avoiding an LRU scan cycle.
            while cache.len() > 256
                || cache
                    .iter()
                    .map(|(s, p)| s.len() + p.source.len())
                    .sum::<usize>()
                    > 4 * 1024 * 1024
            {
                cache.pop_front();
            }
        }
        self.check_ms += started.elapsed().as_secs_f64() * 1000.0;
        checked.map(|p| (p, template))
    }
    pub fn evaluate(&mut self, expression: &str, ty: Option<&Ty>) -> Result<(Data, Ty), EditError> {
        let body = format!(
            "{}probe =\n{}\n",
            ty.map(|t| format!("probe :: {}\n", t.source()))
                .unwrap_or_default(),
            indent(expression)
        );
        self.execute(&body, &[])
    }
    fn execute(&mut self, body: &str, arguments: &[&str]) -> Result<(Data, Ty), EditError> {
        let (value, ty) = self.execute_lazy(body, arguments)?;
        Ok((self.decode_live(&value)?, ty))
    }
    pub fn add_validation(
        &mut self,
        batch: &mut Vec<(String, String)>,
        bytes: &mut usize,
        declarations: String,
        predicate: String,
    ) -> Result<(), EditError> {
        let size = declarations.len() + predicate.len();
        if !batch.is_empty() && (batch.len() >= 16 || *bytes + size > self.limits.source_bytes / 2)
        {
            self.flush_validation(batch)?;
            *bytes = 0;
        }
        *bytes += size;
        batch.push((declarations, predicate));
        Ok(())
    }
    pub fn flush_validation(&mut self, batch: &mut Vec<(String, String)>) -> Result<(), EditError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.declarations = batch.iter().map(|(d, _)| d.as_str()).collect::<String>();
        let condition = batch
            .iter()
            .map(|(_, p)| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" && ");
        self.validating = true;
        let result = self.boolean(&condition);
        self.validating = false;
        if !matches!(result, Ok(true)) {
            self.validations.pending = None;
        }
        if !result? {
            return Err(EditError::ValidationFailed);
        }
        self.validations.commit(
            self.limits.traversal.retained_values,
            self.limits.traversal.compiled_bytes,
        );
        batch.clear();
        Ok(())
    }
    pub fn boolean(&mut self, e: &str) -> Result<bool, EditError> {
        let (v, _) = self.evaluate(e, Some(&Ty::Named("Bool".into(), vec![])))?;
        match v {
            Data::Constructor(n, _) if n.module == ["Prelude"] => Ok(n.name == "True"),
            _ => Err(EditError::Internal("non-Bool probe".into())),
        }
    }
}
fn operation_error(error: crate::operation::CallError) -> EditError {
    match error {
        crate::operation::CallError::Runtime(outcome)
        | crate::operation::CallError::Extension(twine::ExtensionError::Runtime(outcome)) => {
            outcome.into()
        }
        other => EditError::Internal(format!("checked operation: {other:?}")),
    }
}
fn decode(
    s: &mut twine::Session,
    h: ValueHandle,
    depth: usize,
    nodes: &mut usize,
) -> Result<Data, EditError> {
    *nodes += 1;
    if depth > 128 || *nodes > 10000 {
        return unsupported("request exceeds bounded materialization (128 depth / 10000 values)");
    }
    let (tag, len) = match s.view(h)? {
        ValueView::Literal(l) => return Ok(Data::Literal(l.clone())),
        ValueView::Constructor(n, len) => (Data::Constructor(n.clone(), vec![]), len),
        ValueView::Tuple(len) => (Data::Tuple(vec![]), len),
        ValueView::Record(fs) => (
            Data::Record(
                fs.iter()
                    .map(|f| (f.to_string(), Data::Tuple(vec![])))
                    .collect(),
            ),
            fs.len(),
        ),
        _ => return unsupported("functions, native values and IO have no source materializer"),
    };
    let mut values = vec![];
    for c in s.children(h, 0, len)? {
        // Materialization is transaction work, not an interactive inspection.
        // Force on the owning session so all fields consume one cumulative
        // budget and cancellation/budget outcomes retain their real category.
        let v = s.force(c.handle)?;
        values.push(decode(s, v, depth + 1, nodes)?);
    }
    Ok(match tag {
        Data::Constructor(n, _) => Data::Constructor(n, values),
        Data::Tuple(_) => Data::Tuple(values),
        Data::Record(fs) => Data::Record(fs.into_keys().zip(values).collect()),
        _ => unreachable!(),
    })
}
pub(super) fn indent(s: &str) -> String {
    s.lines().map(|l| format!("  {l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoded_queries_share_runtime_memoization_and_the_live_code_budget() {
        let dir = std::env::temp_dir().join(format!("knot-query-unified-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Main.knot");
        std::fs::write(&path, "module Main exposing (..)\ncost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\n").unwrap();
        let snapshot =
            ProjectSnapshot::load(&path, &[], &Default::default(), None, &Default::default())
                .unwrap();
        let library = crate::libraries::Libraries::load(None).unwrap().0.core;
        let context = Arc::new(ProbeContext::new(&snapshot, &library).unwrap());
        assert!(Arc::ptr_eq(
            &context.imports.interfaces()[0],
            &snapshot.graph.entry().interface
        ));
        let ty = Ty::Named("Int".into(), vec![]);
        let mut probes = Probes::from_context(context.clone(), EditLimits::default());
        assert_eq!(
            probes.evaluate("Main.cost 100", Some(&ty)).unwrap().0,
            Data::Literal(knot_core::Literal::Int(100))
        );
        let initial = probes.usage.steps;
        let heap = probes.usage.heap_entries;
        assert_eq!(
            probes.evaluate("Main.cost 100", Some(&ty)).unwrap().0,
            Data::Literal(knot_core::Literal::Int(100))
        );
        assert!(probes.usage.steps - initial < initial / 10);
        assert_eq!(probes.usage.heap_entries, heap);
        // A new transaction shares checked code, not a hidden decoded value or
        // an old runtime heap. It must still execute under its own limits.
        let mut limits = EditLimits::default();
        limits.runtime.steps = initial / 2;
        assert!(matches!(
            Probes::from_context(context.clone(), limits).evaluate("Main.cost 100", Some(&ty)),
            Err(EditError::Runtime(twine::Outcome::BudgetExhausted))
        ));
        let mut limits = EditLimits::default();
        limits.traversal.compiled_bytes = 1;
        assert!(matches!(
            Probes::from_context(context, limits).evaluate("Main.cost 100", Some(&ty)),
            Err(EditError::SourceTooLarge)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn decoded_and_lazy_materialization_share_one_execution_budget() {
        let dir = std::env::temp_dir().join(format!("knot-material-budget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Main.knot");
        std::fs::write(&path, "module Main exposing (entry)\nentry = cost 1\ncost :: Int -> Int\ncost n = if n == 0 then 0 else 1 + cost (n - 1)\n").unwrap();
        let snapshot =
            ProjectSnapshot::load(&path, &[], &Default::default(), None, &Default::default())
                .unwrap();
        let library = crate::libraries::Libraries::load(None).unwrap().0.core;
        let context = Arc::new(ProbeContext::new(&snapshot, &library).unwrap());
        assert!(!snapshot
            .graph
            .entry()
            .interface
            .exports
            .values
            .contains("cost"));
        let int = Ty::Named("Int".into(), vec![]);
        let pair = Ty::Tuple(vec![int.clone(), int.clone()]);
        let expression = "(Main.cost 100, Main.cost 101)";
        let mut single = Probes::from_context(context.clone(), EditLimits::default());
        single.evaluate("Main.cost 100", Some(&int)).unwrap();
        let mut both = Probes::from_context(context.clone(), EditLimits::default());
        both.evaluate(expression, Some(&pair)).unwrap();
        assert!(both.usage.steps > single.usage.steps * 3 / 2);
        let allowance = single.usage.steps + (both.usage.steps - single.usage.steps) / 2;
        // Shared checked modules must not bypass a fresh runtime's budget.
        let mut limits = EditLimits::default();
        limits.runtime.steps = allowance;
        let mut limited = Probes::from_context(context.clone(), limits);
        assert!(matches!(
            limited.evaluate(expression, Some(&pair)),
            Err(EditError::Runtime(twine::Outcome::BudgetExhausted))
        ));
        let mut limits = EditLimits::default();
        limits.runtime.steps = allowance;
        let mut live = Probes::from_context(context.clone(), limits);
        let (value, _) = live.evaluate_lazy(expression, Some(&pair)).unwrap();
        assert!(matches!(
            live.decode_live(&value),
            Err(EditError::Runtime(twine::Outcome::BudgetExhausted))
        ));
        let limits = EditLimits::default();
        let mut cancelled = Probes::from_context(context, limits.clone());
        let (value, _) = cancelled.evaluate_lazy(expression, Some(&pair)).unwrap();
        limits
            .runtime
            .cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            cancelled.decode_live(&value),
            Err(EditError::Runtime(twine::Outcome::Cancelled))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unified_queries_preserve_scalars_patterns_and_runtime_limits() {
        let dir = std::env::temp_dir().join(format!("knot-probe-template-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Main.knot");
        std::fs::write(
            &path,
            "module Main exposing (..)\nprogram :: Int\nprogram = 1\n",
        )
        .unwrap();
        let snapshot =
            ProjectSnapshot::load(&path, &[], &Default::default(), None, &Default::default())
                .unwrap();
        let library = crate::libraries::Libraries::load(None).unwrap().0.core;
        let context = Arc::new(ProbeContext::new(&snapshot, &library).unwrap());
        let mut probes = Probes::from_context(context.clone(), EditLimits::default());
        let int = Some(Ty::Named("Int".into(), vec![]));
        for (expression, expected) in [
            ("case 17 of\n  17 -> 39\n  _ -> 12", 39),
            ("case 19 of\n  17 -> 39\n  _ -> 12", 12),
            ("case 19 of\n  19 -> 39\n  _ -> 12", 39),
        ] {
            let (value, _) = probes.evaluate(expression, int.as_ref()).unwrap();
            assert_eq!(value, Data::Literal(knot_core::Literal::Int(expected)));
        }
        for text in ["é\n\"\\", "a considerably longer value", ""] {
            let (value, _) = probes
                .evaluate(&quoted(text), Some(&Ty::Named("String".into(), vec![])))
                .unwrap();
            assert_eq!(
                value,
                Data::Literal(knot_core::Literal::String(text.into()))
            );
        }
        for (text, expected) in [("-1.25", -1.25), ("2.75e2", 275.0)] {
            let (value, _) = probes
                .evaluate(text, Some(&Ty::Named("Float".into(), vec![])))
                .unwrap();
            assert_eq!(value, Data::Literal(knot_core::Literal::Float(expected)));
        }
        let ty = Ty::Tuple(vec![
            Ty::Named("Int".into(), vec![]),
            Ty::Named("String".into(), vec![]),
            Ty::Named("Float".into(), vec![]),
        ]);
        for (n, text, f) in [(1, "é", 1.25), (999999, "longer unicode 😄", 25.5)] {
            let expression = format!("({n}, {}, {f:?})", quoted(text));
            let (value, _) = probes.evaluate(&expression, Some(&ty)).unwrap();
            assert_eq!(
                value,
                Data::Tuple(vec![
                    Data::Literal(knot_core::Literal::Int(n)),
                    Data::Literal(knot_core::Literal::String(text.into())),
                    Data::Literal(knot_core::Literal::Float(f))
                ])
            );
        }
        let before = probes.cache_hits;
        probes
            .evaluate("-1.25", Some(&Ty::Named("Float".into(), vec![])))
            .unwrap();
        assert!(probes.cache_hits > before);
        let mut limits = EditLimits::default();
        limits.runtime.steps = 0;
        assert!(Probes::from_context(context.clone(), limits)
            .evaluate("-1.25", Some(&Ty::Named("Float".into(), vec![])))
            .is_err());
        let mut limits = EditLimits::default();
        limits.runtime.primitive_bytes = 1;
        assert!(Probes::from_context(context.clone(), limits)
            .evaluate(
                &quoted("a considerably longer value"),
                Some(&Ty::Named("String".into(), vec![]))
            )
            .is_err());
        let limits = EditLimits::default();
        limits
            .runtime
            .cancellation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(Probes::from_context(context, limits)
            .evaluate("-1.25", Some(&Ty::Named("Float".into(), vec![])))
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn pooled_operations_keep_fresh_arguments_and_do_not_normalize_strings_or_locals() {
        let dir = std::env::temp_dir().join(format!("knot-probe-pool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Main.knot");
        std::fs::write(
            &path,
            "module Main exposing (..)\nprogram :: Int\nprogram = 1\n",
        )
        .unwrap();
        let snapshot =
            ProjectSnapshot::load(&path, &[], &Default::default(), None, &Default::default())
                .unwrap();
        let library = crate::libraries::Libraries::load(None).unwrap().0.core;
        let context = Arc::new(ProbeContext::new(&snapshot, &library).unwrap());
        let mut probes = Probes::from_context(context, EditLimits::default());
        let int = Ty::Named("Int".into(), vec![]);
        let string = Ty::Named("String".into(), vec![]);
        let mut previous = None;
        for n in [3, 7, 11] {
            probes.declarations.clear();
            let (input, _) = probes.evaluate_lazy(&n.to_string(), Some(&int)).unwrap();
            let reference = probes.reference(&input).unwrap();
            probes.declarations = format!("value :: Int\nvalue = (( {reference} ))\n");
            let hits = probes.cache_hits;
            let (result, _) = probes.evaluate_lazy("value + 1", Some(&int)).unwrap();
            assert_eq!(
                probes.decode_live(&result).unwrap(),
                Data::Literal(knot_core::Literal::Int(n + 1))
            );
            if previous.is_some() {
                assert!(probes.cache_hits > hits);
            }
            previous = Some(n);
            let hits = probes.cache_hits;
            let (result, _) = probes
                .evaluate_lazy(&quoted(&reference), Some(&string))
                .unwrap();
            assert_eq!(
                probes.decode_live(&result).unwrap(),
                Data::Literal(knot_core::Literal::String(reference.clone()))
            );
            assert_eq!(probes.cache_hits, hits, "string contents must remain exact");
            let (result, _) = probes
                .evaluate_lazy(
                    &format!("(\\{reference} -> {reference} + 1) 40"),
                    Some(&int),
                )
                .unwrap();
            assert_eq!(
                probes.decode_live(&result).unwrap(),
                Data::Literal(knot_core::Literal::Int(41))
            );
            assert_eq!(
                probes.cache_hits, hits,
                "shadowed local names are not retained parameters"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
