//! Disposable on-disk cache of bundled library core, keyed by the embedding
//! compiler build and bundled sources. No host callbacks or user evaluation results.
use bincode::Options;
use knot_checker::{check::check_library_program, program::ModuleRevisionId};
use knot_core::{ExprId, Kind, Program};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::Path,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Libraries {
    pub core: Program,
    pub sources: Vec<(ModuleRevisionId, String, String)>,
}
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_limit(32 * 1024 * 1024)
        .reject_trailing_bytes()
}

fn build() -> Result<Libraries, String> {
    let mut modules = vec![];
    let mut sources = vec![];
    for (name, source) in knot_canonical::prelude::LIBRARY {
        let module = check_library_program(name).map_err(|e| format!("library {name}: {e:?}"))?;
        sources.push((module.revision, format!("{name}.knot"), source.to_string()));
        modules.push(module);
    }
    Ok(Libraries {
        core: knot_core::lower(&modules),
        sources,
    })
}
fn key() -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    env!("KNOT_CORE_CACHE_BUILD_ID").hash(&mut hash);
    for (name, source) in knot_canonical::prelude::LIBRARY {
        name.hash(&mut hash);
        source.hash(&mut hash);
    }
    hash.finish()
}
fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}
fn decode(bytes: &[u8]) -> Option<Libraries> {
    let (header, payload) = bytes.split_at_checked(8)?;
    if u64::from_le_bytes(header.try_into().ok()?) != checksum(payload) {
        return None;
    }
    codec().deserialize(payload).ok()
}
fn valid(l: &Libraries) -> bool {
    let size = l.core.expressions.len();
    let index = |id: &ExprId| id.0 < size;
    if !l.core.globals.values().all(index) {
        return false;
    }
    if l.core
        .local_names
        .keys()
        .any(|id| id.0 >= l.core.next_local)
    {
        return false;
    }
    for e in &l.core.expressions {
        if !l
            .sources
            .iter()
            .any(|(r, _, _)| *r == e.origin.node.revision)
        {
            return false;
        }
        let good = match &e.kind {
            Kind::Lam(_, b) | Kind::Select(b, _) => index(b),
            Kind::App(f, xs) => index(f) && xs.iter().all(index),
            Kind::Let(bs, b) => index(b) && bs.iter().all(|(_, v)| index(v)),
            Kind::Case(x, bs) => index(x) && bs.iter().all(|(_, b)| index(b)),
            Kind::Tuple(xs) => xs.iter().all(index),
            Kind::Record(fs) => fs.iter().all(|(_, v)| index(v)),
            Kind::RecordUpdate(x, fs) => index(x) && fs.iter().all(|(_, v)| index(v)),
            Kind::Dict(fs) => fs.iter().all(|(_, v)| index(v)),
            _ => true,
        };
        if !good {
            return false;
        }
    }
    true
}
impl Libraries {
    /// One immutable compilation of the bundled sources per process. The first
    /// caller may supply a disk-cache hint; later callers share that checked
    /// artifact and its source identities. Caller mutations detach Core tables.
    pub fn bundled(directory: Option<&Path>) -> Result<Self, String> {
        static BUNDLED: std::sync::OnceLock<Result<Libraries, String>> = std::sync::OnceLock::new();
        BUNDLED
            .get_or_init(|| Self::load(directory).map(|(libraries, _)| libraries))
            .clone()
    }
    /// Returns (libraries, cache hit). Unreadable/corrupt/stale caches are misses;
    /// failed cache writes never prevent compilation. `None` disables disk use.
    pub fn load(directory: Option<&Path>) -> Result<(Self, bool), String> {
        let path = directory.map(|dir| dir.join(format!("libraries-{:016x}.bin", key())));
        if let Some(path) = &path {
            if std::fs::metadata(path).is_ok_and(|m| m.len() <= 32 * 1024 * 1024) {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Some(mut cached) = decode(&bytes) {
                        if valid(&cached) {
                            let revisions: HashMap<_, _> = cached
                                .sources
                                .iter()
                                .map(|(r, _, _)| (*r, ModuleRevisionId::fresh()))
                                .collect();
                            for e in &mut cached.core.expressions {
                                e.origin.node.revision = revisions[&e.origin.node.revision];
                            }
                            for (r, _, _) in &mut cached.sources {
                                *r = revisions[r];
                            }
                            return Ok((cached, true));
                        }
                    }
                }
            }
        }
        let result = build()?;
        if let Some(path) = path {
            if let Ok(payload) = codec().serialize(&result) {
                let mut bytes = Vec::with_capacity(payload.len() + 8);
                bytes.extend_from_slice(&checksum(&payload).to_le_bytes());
                bytes.extend_from_slice(&payload);
                if let Some(parent) = path.parent() {
                    if std::fs::create_dir_all(parent).is_ok() {
                        // Unique temporary files plus atomic rename make concurrent
                        // writers harmless. Readers never see partial artifacts.
                        use std::io::Write;
                        static NEXT: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let tmp =
                            path.with_extension(format!("{}-{serial}.tmp", std::process::id()));
                        if let Ok(mut f) = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&tmp)
                        {
                            if f.write_all(&bytes).and_then(|_| f.sync_all()).is_ok() {
                                let _ = std::fs::rename(&tmp, &path);
                            }
                            let _ = std::fs::remove_file(tmp);
                        }
                    }
                }
            }
        }
        Ok((result, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_metadata_preserves_flat_cache_encoding() {
        let libraries = build().unwrap();
        let expressions = &libraries.core.expressions;
        let flat: Vec<_> = expressions.iter().collect();
        let bytes = codec().serialize(&flat).unwrap();
        assert_eq!(codec().serialize(expressions).unwrap(), bytes);
        let restored: knot_core::Expressions = codec().deserialize(&bytes).unwrap();
        assert_eq!(restored.len(), expressions.len());
        assert_eq!(codec().serialize(&restored).unwrap(), bytes);
        let names = &libraries.core.local_names;
        let flat: std::collections::BTreeMap<_, _> = names.iter().collect();
        let bytes = codec().serialize(&flat).unwrap();
        assert_eq!(codec().serialize(names).unwrap(), bytes);
        let restored: knot_core::LocalNames = codec().deserialize(&bytes).unwrap();
        assert_eq!(
            restored.iter().collect::<Vec<_>>(),
            names.iter().collect::<Vec<_>>()
        );
        assert_eq!(codec().serialize(&restored).unwrap(), bytes);
    }
}
