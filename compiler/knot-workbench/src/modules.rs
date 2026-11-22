//! Local project loading with conservative reuse of unchanged checked imports.
use knot_canonical::{
    ast::CType,
    env::{CtorInfo, ModuleInterface, ModuleRegistry},
};
use knot_checker::{
    host::HostDeclarations,
    modules::{exported_names, select_exports, CheckedInterface},
    program::TypedProgram,
};
use knot_syntax::ast::decl::{Decl, Fixity, Import, Module};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Extra imports are added to the AST; suffix declarations are appended verbatim.
/// Original source bytes and their offsets never move.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EntryExtension {
    pub imports: Vec<(Vec<String>, String)>,
    pub suffix: String,
}

#[derive(Clone)]
pub struct LoadedModule {
    pub path: PathBuf,
    pub source: String,
    pub program: Arc<TypedProgram>,
    pub interface: Arc<CheckedInterface>,
    // The import header is sufficient to re-resolve byte-identical dependency sources.
    imports: Arc<[Import]>,
    pub(crate) exposes_all: bool,
    /// Transitive user dependencies, in deterministic dependency order.
    pub dependencies: Vec<Vec<String>>,
}
pub struct ModuleGraph {
    /// Dependencies precede consumers. The entry module is last.
    pub modules: Vec<LoadedModule>,
    hosts: HostDeclarations,
}
/// The browser mounts a flat, symlink-free project directory at an absolute path.
pub fn module_path(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "wasi")]
    {
        std::fs::metadata(path)?;
        Ok(path.to_owned())
    }
    #[cfg(not(target_os = "wasi"))]
    path.canonicalize()
}

impl ModuleGraph {
    /// Defaults to the entry file's directory if roots is empty. With explicit
    /// roots, all imports must resolve uniquely beneath one of those roots.
    pub fn load(entry: &Path, roots: &[PathBuf], hosts: &HostDeclarations) -> Result<Self, String> {
        Self::load_with_source(entry, roots, hosts, None)
    }
    /// An editor/CLI can replace the entry source without writing a temporary file.
    pub fn load_with_source(
        entry: &Path,
        roots: &[PathBuf],
        hosts: &HostDeclarations,
        source: Option<String>,
    ) -> Result<Self, String> {
        Self::load_extended(entry, roots, hosts, source, &EntryExtension::default())
    }

    pub fn load_extended(
        entry: &Path,
        roots: &[PathBuf],
        hosts: &HostDeclarations,
        source: Option<String>,
        extension: &EntryExtension,
    ) -> Result<Self, String> {
        Self::load_reusing(entry, roots, hosts, source, extension, None)
    }

    /// Re-resolve and read imports on every load. Only checked imports with
    /// identical source, path, host declarations and transitive dependencies
    /// are reused. The entry is always checked, including generated extensions.
    pub fn load_reusing(
        entry: &Path,
        roots: &[PathBuf],
        hosts: &HostDeclarations,
        source: Option<String>,
        extension: &EntryExtension,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        let previous = previous
            .filter(|p| p.hosts.types == hosts.types && p.hosts.functions == hosts.functions);
        let entry = match module_path(entry) {
            Ok(path) => path,
            Err(error)
                if source.is_some()
                    && error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(entry).is_err() =>
            {
                // Unsaved buffers still need a real parent for import resolution.
                let parent = entry
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                module_path(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?
                    .join(entry.file_name().ok_or("entry needs a file name")?)
            }
            Err(error) => return Err(format!("{}: {error}", entry.display())),
        };
        let roots = if roots.is_empty() {
            vec![entry.parent().expect("file parent").to_owned()]
        } else {
            roots
                .iter()
                .map(|p| {
                    module_path(p)
                        .map_err(|e| format!("{}: {e}", p.display()))
                })
                .collect::<Result<_, _>>()?
        };
        let source = match source {
            Some(s) => s,
            None => read(&entry)?,
        };
        let source = source + &extension.suffix;
        let mut raw = parse(&entry, &source)?;
        for (module, alias) in &extension.imports {
            raw.imports.push(knot_syntax::ast::decl::Import {
                module: module.clone(),
                alias: Some(alias.clone()),
                exposing: None,
            });
        }
        let mut loader = Loader {
            roots,
            hosts,
            previous,
            modules: Vec::new(),
            indices: BTreeMap::new(),
            stack: Vec::new(),
            bundled: BTreeMap::new(),
        };
        for (name, raw) in knot_canonical::prelude::library_modules() {
            if hosts.exports(&raw.name).is_some() {
                return Err(format!(
                    "host module {name} collides with a bundled library"
                ));
            }
            loader
                .bundled
                .insert(raw.name.clone(), exported_names(raw)?);
        }
        loader.visit(entry, source, ModuleInput::Parsed(raw))?;
        Ok(Self {
            modules: loader.modules,
            hosts: hosts.clone(),
        })
    }
    /// Prepare a bounded inspector expression in any module's lexical scope.
    /// Replace that module's typed program before linking a new session. As with
    /// single-file queries, this does not attach code to an existing session.
    pub fn prepare_query(
        &self,
        module: &[String],
        target: knot_syntax::span::Span,
        expression: &str,
    ) -> Result<crate::query::PreparedQuery, String> {
        let selected = self
            .modules
            .iter()
            .find(|m| m.interface.name == module)
            .ok_or("module not loaded")?;
        let imports: Vec<_> = self
            .modules
            .iter()
            .filter(|m| selected.dependencies.contains(&m.interface.name))
            .map(|m| m.interface.as_ref())
            .collect();
        crate::query::prepare_with_imports(
            &selected.source,
            target,
            expression,
            &self.hosts,
            &Registry {
                modules: &self.modules,
                hosts: &self.hosts,
            },
            &imports,
        )
    }

    pub fn entry(&self) -> &LoadedModule {
        self.modules.last().expect("entry module")
    }
    pub fn entry_mut(&mut self) -> &mut LoadedModule {
        self.modules.last_mut().expect("entry module")
    }
}
fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn parse(path: &Path, source: &str) -> Result<Module, String> {
    knot_syntax::parse(source).map_err(|e| format!("{}: {}", path.display(), e.render(source)))
}
enum ModuleInput<'a> {
    Parsed(Module),
    Checked(&'a LoadedModule),
}

struct Loader<'a> {
    roots: Vec<PathBuf>,
    hosts: &'a HostDeclarations,
    previous: Option<&'a ModuleGraph>,
    modules: Vec<LoadedModule>,
    indices: BTreeMap<Vec<String>, usize>,
    stack: Vec<Vec<String>>,
    bundled: BTreeMap<Vec<String>, ModuleInterface>,
}
impl Loader<'_> {
    fn find(&self, name: &[String]) -> Result<PathBuf, String> {
        let relative = name.iter().collect::<PathBuf>().with_extension("knot");
        let mut matches = BTreeSet::new();
        for root in &self.roots {
            let candidate = root.join(&relative);
            match module_path(&candidate) {
                Ok(p) if p.is_file() => {
                    matches.insert(p);
                }
                Ok(_) => return Err(format!("{} is not a module file", candidate.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", candidate.display())),
            }
        }
        match matches.len() {
            0 => Err(format!(
                "missing module {} (expected {})",
                name.join("."),
                relative.display()
            )),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => Err(format!("ambiguous module {}: {matches:?}", name.join("."))),
        }
    }
    fn dependency(&mut self, name: &[String]) -> Result<(), String> {
        if self.stack.iter().any(|n| n == name) {
            return Err(format!(
                "cyclic module import: {} -> {}",
                self.trace(),
                name.join(".")
            ));
        }
        if self.indices.contains_key(name)
            || self.bundled.contains_key(name)
            || self.hosts.exports(name).is_some()
        {
            return Ok(());
        }
        let path = self
            .find(name)
            .map_err(|e| format!("{e}\nimport trace: {} -> {}", self.trace(), name.join(".")))?;
        let trace = format!("{} -> {}", self.trace(), name.join("."));
        let result = (|| {
            let source = read(&path)?;
            // A previous entry may have synthetic imports. Only dependency modules
            // have headers and checked programs derived solely from these disk bytes.
            let previous = self.previous.and_then(|g| {
                g.modules
                    .split_last()?
                    .1
                    .iter()
                    .find(|m| m.path == path && m.interface.name == name && m.source == source)
            });
            let input = if let Some(old) = previous {
                ModuleInput::Checked(old)
            } else {
                let raw = parse(&path, &source)?;
                if raw.name != name {
                    return Err(format!(
                        "{}: requested {}, but declares {}",
                        path.display(),
                        name.join("."),
                        raw.name.join(".")
                    ));
                }
                ModuleInput::Parsed(raw)
            };
            self.visit(path, source, input)
        })();
        result.map_err(|e| {
            if e.contains("import trace:") || e.contains("cyclic module import:") {
                e
            } else {
                format!("{e}\nimport trace: {trace}")
            }
        })
    }

    fn trace(&self) -> String {
        self.stack
            .iter()
            .map(|n| n.join("."))
            .collect::<Vec<_>>()
            .join(" -> ")
    }
    fn visit(
        &mut self,
        path: PathBuf,
        source: String,
        input: ModuleInput<'_>,
    ) -> Result<(), String> {
        let (name, imports) = match &input {
            ModuleInput::Parsed(raw) => {
                if self.bundled.contains_key(&raw.name) || self.hosts.exports(&raw.name).is_some() {
                    return Err(format!(
                        "{}: module {} collides with a reserved bundled/host module",
                        path.display(),
                        raw.name.join(".")
                    ));
                }
                exported_names(raw).map_err(|e| format!("{}: {e}", path.display()))?;
                (&raw.name, raw.imports.as_slice())
            }
            ModuleInput::Checked(old) => (&old.interface.name, old.imports.as_ref()),
        };
        self.stack.push(name.clone());
        let mut qualifiers = BTreeSet::new();
        for import in imports {
            let qualifier = import
                .alias
                .clone()
                .unwrap_or_else(|| import.module.join("."));
            if !qualifiers.insert(qualifier.clone()) {
                return Err(format!(
                    "{}: duplicate import qualifier {qualifier}",
                    path.display()
                ));
            }
            self.dependency(&import.module)?;
            if let Some(exposing) = &import.exposing {
                select_exports(
                    &Registry {
                        modules: &self.modules,
                        hosts: self.hosts,
                    }
                    .exports(&import.module)
                    .expect("loaded import"),
                    exposing,
                )
                .map_err(|e| {
                    format!(
                        "{}: import {}: {e}",
                        path.display(),
                        import.module.join(".")
                    )
                })?;
            }
        }
        let mut needed = BTreeSet::new();
        for import in imports {
            if let Some(i) = self.indices.get(&import.module) {
                needed.insert(import.module.clone());
                needed.extend(self.modules[*i].dependencies.iter().cloned());
            }
        }
        let dependencies: Vec<_> = self
            .modules
            .iter()
            .filter(|m| needed.contains(&m.interface.name))
            .map(|m| m.interface.name.clone())
            .collect();
        let raw = match input {
            ModuleInput::Parsed(raw) => raw,
            ModuleInput::Checked(old) => {
                let unchanged = old.dependencies == dependencies
                    && dependencies.iter().all(|name| {
                        self.previous
                            .and_then(|g| g.modules.iter().find(|m| &m.interface.name == name))
                            .is_some_and(|m| {
                                Arc::ptr_eq(&m.program, &self.modules[self.indices[name]].program)
                            })
                    });
                if unchanged {
                    self.stack.pop();
                    self.indices
                        .insert(old.interface.name.clone(), self.modules.len());
                    self.modules.push(old.clone());
                    return Ok(());
                }
                parse(&path, &source)?
            }
        };
        let canonical = knot_canonical::canonicalize_module(
            &raw,
            Some(&Registry {
                modules: &self.modules,
                hosts: self.hosts,
            }),
        )
        .map_err(|errors| {
            errors
                .iter()
                .map(|e| diagnostic(&path, &source, e.span, format!("{:?}", e.kind)))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        let interfaces: Vec<_> = dependencies
            .iter()
            .map(|n| self.modules[self.indices[n]].interface.as_ref())
            .collect();
        let program =
            knot_checker::check::check_program_with_imports(&canonical, self.hosts, &interfaces)
                .map_err(|report| {
                    report
                        .errors
                        .iter()
                        .map(|e| diagnostic(&path, &source, e.span, format!("{:?}", e.kind)))
                        .collect::<Vec<_>>()
                        .join("\n")
                })?;
        let interface = CheckedInterface::from_program(&program, &raw)?;
        self.stack.pop();
        self.indices.insert(raw.name, self.modules.len());
        self.modules.push(LoadedModule {
            path,
            source,
            program: Arc::new(program),
            interface: Arc::new(interface),
            imports: raw.imports.into(),
            exposes_all: matches!(raw.exposing, knot_syntax::ast::decl::Exposing::All),
            dependencies,
        });
        Ok(())
    }
}
fn diagnostic(path: &Path, source: &str, span: knot_syntax::span::Span, message: String) -> String {
    let (line, col) = knot_syntax::line_index::LineIndex::new(source).line_col(span.start);
    let excerpt = source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("");
    format!(
        "{}:{line}:{col}: {message}\n  {}",
        path.display(),
        excerpt.chars().take(160).collect::<String>()
    )
}
struct Registry<'a> {
    modules: &'a [LoadedModule],
    hosts: &'a HostDeclarations,
}
impl Registry<'_> {
    fn interface(&self, module: &[String]) -> Option<&CheckedInterface> {
        self.modules
            .iter()
            .find(|m| m.interface.name == module)
            .map(|m| m.interface.as_ref())
    }
}
impl ModuleRegistry for Registry<'_> {
    fn exports(&self, module: &[String]) -> Option<ModuleInterface> {
        self.interface(module)
            .map(|i| i.exports.clone())
            .or_else(|| {
                knot_canonical::prelude::library_modules()
                    .iter()
                    .find(|(_, m)| m.name == module)
                    .map(|(_, m)| exported_names(m).expect("bundled exports"))
            })
            .or_else(|| self.hosts.exports(module))
    }
    fn aliases(&self, module: &[String]) -> HashMap<String, (Vec<String>, CType)> {
        self.interface(module)
            .map(|i| i.aliases.clone())
            .unwrap_or_default()
    }
    fn fixity(&self, module: &[String], name: &str) -> Option<Fixity> {
        self.interface(module)
            .and_then(|i| i.fixities.get(name).copied())
            .or_else(|| {
                if module == ["Prelude"] {
                    knot_canonical::prelude::builtin_fixity(name)
                } else {
                    None
                }
            })
    }
    fn constructor(&self, module: &[String], name: &str) -> Option<CtorInfo> {
        self.interface(module)
            .and_then(|i| i.constructors.get(name).cloned())
            .or_else(|| {
                let (_, raw) = knot_canonical::prelude::library_modules()
                    .iter()
                    .find(|(_, m)| m.name == module)?;
                for d in &raw.decls {
                    if let Decl::TypeDecl(n, _, variants, _) = &d.node {
                        if let Some((_, fields)) = variants.iter().find(|(c, _)| c == name) {
                            return Some(CtorInfo {
                                arity: fields.len(),
                                type_name: n.clone(),
                            });
                        }
                    }
                }
                None
            })
    }
}
