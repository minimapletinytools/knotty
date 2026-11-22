//! Successful Bool witnesses with exact argument identity and static executable
//! dependency proofs. No decoded values or source-span invalidation heuristics.
//! This cache belongs to one Probes session: candidate installation proves the
//! same value/type/instance ABI, and retained arguments keep their old captures.
use super::*;
use knot_core::{Global, Kind, Program};
use std::{collections::BTreeSet, sync::OnceLock};

pub(super) struct Proof {
    checked: Arc<knot_checker::program::TypedProgram>,
    output: String,
    inputs: BTreeSet<String>,
    dependencies: OnceLock<Option<BTreeSet<Global>>>,
}
impl Proof {
    pub fn new(
        checked: Arc<knot_checker::program::TypedProgram>,
        output: &str,
        inputs: &[&str],
    ) -> Arc<Self> {
        Arc::new(Self {
            checked,
            output: output.into(),
            inputs: inputs.iter().map(|s| (*s).into()).collect(),
            dependencies: Default::default(),
        })
    }
    fn dependencies(&self, remaining: &mut usize) -> &Option<BTreeSet<Global>> {
        self.dependencies.get_or_init(|| {
            if *remaining == 0 {
                return None;
            }
            let core = knot_core::lower(std::slice::from_ref(self.checked.as_ref()));
            let module = &self.checked.canonical.name;
            let root = Global::Value(GlobalName {
                module: module.clone(),
                name: self.output.clone(),
            });
            let mut pending = vec![*core.globals.get(&root)?];
            let mut seen = BTreeSet::new();
            let mut globals = BTreeSet::new();
            while let Some(id) = pending.pop() {
                if !seen.insert(id) {
                    continue;
                }
                *remaining = remaining.checked_sub(1)?;
                match &core.expressions[id.0].kind {
                    Kind::Global(global) => {
                        if matches!(global, Global::Value(name) if &name.module == module && self.inputs.contains(&name.name)) {
                            continue; // supplied immutable session values, not the stubs
                        }
                        if let Some(target) = core.globals.get(global) {
                            pending.push(*target);
                        } else {
                            globals.insert(global.clone());
                        }
                    }
                    Kind::Lam(_, body) | Kind::Select(body, _) => pending.push(*body),
                    Kind::App(fun, args) => {
                        pending.push(*fun);
                        pending.extend(args);
                    }
                    Kind::Let(bindings, body) => {
                        pending.push(*body);
                        pending.extend(bindings.iter().map(|(_, value)| value));
                    }
                    Kind::Case(value, arms) => {
                        pending.push(*value);
                        pending.extend(arms.iter().map(|(_, body)| body));
                    }
                    Kind::Tuple(parts) => pending.extend(parts),
                    Kind::Record(fields) => pending.extend(fields.iter().map(|(_, value)| value)),
                    Kind::RecordUpdate(base, fields) => {
                        pending.push(*base);
                        pending.extend(fields.iter().map(|(_, value)| value));
                    }
                    Kind::Dict(fields) => pending.extend(fields.iter().map(|(_, value)| value)),
                    Kind::Host(..) | Kind::Unsupported(..) => return None,
                    Kind::Local(_) | Kind::Lit(_) | Kind::Rational(_) | Kind::Prim(_) | Kind::Ctor(..) => {}
                }
            }
            Some(globals)
        })
    }
}
pub(super) struct Entry {
    proof: Arc<Proof>,
    inputs: Vec<TypedValue>,
    result: TypedValue,
    base: Arc<Program>,
}
struct Comparison {
    old: Arc<Program>,
    new: Arc<Program>,
    checked: BTreeSet<Global>,
    unchanged: BTreeSet<Global>,
}
#[derive(Default)]
pub(super) struct Cache {
    entries: BTreeMap<String, Vec<Entry>>,
    pub pending: Option<(String, Entry)>,
    comparison: Option<Comparison>,
    bytes: usize,
    count: usize,
    work: usize,
    pub hits: usize,
}
impl Cache {
    pub fn proof(&self, key: &str) -> Option<Arc<Proof>> {
        self.entries
            .get(key)?
            .first()
            .map(|entry| entry.proof.clone())
    }
    pub fn stage(
        &mut self,
        key: String,
        proof: Arc<Proof>,
        inputs: &[(String, TypedValue)],
        result: &TypedValue,
        base: Arc<Program>,
    ) {
        self.pending = Some((
            key,
            Entry {
                proof,
                inputs: inputs.iter().map(|(_, value)| value.clone()).collect(),
                result: result.clone(),
                base,
            },
        ));
    }
    pub fn commit(&mut self, entries: usize, bytes: usize) {
        let Some((key, entry)) = self.pending.take() else {
            return;
        };
        if let Some(previous) = self.entries.get_mut(&key).and_then(|entries| {
            entries.iter_mut().find(|old| {
                old.inputs.len() == entry.inputs.len()
                    && old
                        .inputs
                        .iter()
                        .zip(&entry.inputs)
                        .all(|(a, b)| a.same_thunk(b))
            })
        }) {
            *previous = entry;
            return;
        }
        if self.count >= entries.min(256) || self.bytes.saturating_add(key.len()) > bytes {
            return;
        }
        self.bytes += key.len();
        self.count += 1;
        self.entries.entry(key).or_default().push(entry);
    }
    pub fn reuse(
        &mut self,
        key: &str,
        inputs: &[(String, TypedValue)],
        base: Arc<Program>,
        allowance: usize,
    ) -> Option<TypedValue> {
        let entry = self.entries.get_mut(key)?.iter_mut().find(|entry| {
            inputs.len() == entry.inputs.len()
                && inputs
                    .iter()
                    .zip(&entry.inputs)
                    .all(|((_, a), b)| a.same_thunk(b))
        })?;
        let mut remaining = allowance.saturating_sub(self.work);
        let before = remaining;
        let dependencies = entry.proof.dependencies(&mut remaining);
        self.work += before - remaining;
        let dependencies = dependencies.as_ref()?;
        if self
            .comparison
            .as_ref()
            .is_none_or(|c| !Arc::ptr_eq(&c.old, &entry.base) || !Arc::ptr_eq(&c.new, &base))
        {
            self.comparison = Some(Comparison {
                old: entry.base.clone(),
                new: base.clone(),
                checked: Default::default(),
                unchanged: Default::default(),
            });
        }
        let comparison = self.comparison.as_mut().unwrap();
        let roots = dependencies
            .iter()
            .filter(|g| !comparison.unchanged.contains(*g) && !comparison.checked.contains(*g))
            .cloned()
            .collect::<Vec<_>>();
        let before = remaining;
        comparison
            .unchanged
            .extend(knot_core::equivalence::unchanged_globals_with_budget(
                &entry.base,
                &base,
                &roots,
                &mut remaining,
            ));
        self.work += before - remaining;
        comparison.checked.extend(roots);
        if !dependencies.is_subset(&comparison.unchanged) {
            return None;
        }
        entry.base = base;
        self.hits += 1;
        Some(entry.result.clone())
    }
}
