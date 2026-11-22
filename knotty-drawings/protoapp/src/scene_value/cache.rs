//! Bounded, transactional native output cache. IDs/source text are never
//! invalidation authority: only Twine's complete-data receipts can permit hits.
use super::*;
use std::{collections::HashMap, sync::Arc};
use twine::{DataObservation, DataObservationKey, ObservationBudget};
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;

#[derive(Default)]
pub struct DecodeCache {
    entries: HashMap<DataObservationKey, Arc<Entry>>,
    bytes: usize,
}
struct Entry {
    receipt: DataObservation,
    value: Value,
    nodes: usize,
    text_bytes: usize,
    height: usize,
    bytes: usize,
}
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct DecodeStats {
    pub hits: usize,
    pub skipped_nodes: usize,
    pub attempts: usize,
    pub admitted: usize,
    pub retained_bytes: usize,
    pub receipt_work: u64,
}
pub(super) struct Pass<'a> {
    previous: Option<&'a DecodeCache>,
    next: DecodeCache,
    budget: ObservationBudget,
    stats: DecodeStats,
}
pub fn decode_cached<'a>(
    session: ValueReader<'a>,
    root: ValueHandle,
    previous: Option<&'a DecodeCache>,
) -> Result<(Value, DecodeCache, DecodeStats), String> {
    const WORK: u64 = 1_000_000;
    let mut decoder = Decoder {
        session,
        nodes: 0,
        bytes: 0,
        depths: Vec::new(),
        cache: Some(Pass {
            previous,
            next: DecodeCache::default(),
            budget: ObservationBudget {
                nodes: 4096,
                steps: WORK,
            },
            stats: DecodeStats::default(),
        }),
    };
    let value = decoder.value(root, 0)?;
    let mut pass = decoder.cache.unwrap();
    pass.stats.retained_bytes = pass.next.bytes;
    pass.stats.receipt_work = WORK - pass.budget.steps;
    Ok((value, pass.next, pass.stats))
}
impl Pass<'_> {
    fn insert(&mut self, key: DataObservationKey, entry: Arc<Entry>) {
        if self.next.entries.len() >= MAX_ENTRIES
            || entry.bytes > MAX_BYTES.saturating_sub(self.next.bytes)
        {
            return;
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = self.next.entries.entry(key) {
            self.next.bytes += entry.bytes;
            slot.insert(entry);
        }
    }
}
impl Decoder<'_> {
    pub(super) fn cached(
        &mut self,
        value: ValueHandle,
        depth: usize,
    ) -> Result<Option<Value>, String> {
        let Some(pass) = self.cache.as_mut() else {
            return Ok(None);
        };
        let Some(previous) = pass.previous else {
            return Ok(None);
        };
        let key = self
            .session
            .data_observation_key(value)
            .map_err(|e| format!("{e:?}"))?;
        let Some(entry) = previous.entries.get(&key) else {
            return Ok(None);
        };
        if !self
            .session
            .validate_data_observation(value, &entry.receipt, &mut pass.budget)
            .map_err(|e| format!("{e:?}"))?
        {
            return Ok(None);
        }
        // Cached values must obey the same whole-export limits as ordinary decoding.
        if depth + entry.height > 256 {
            return Err("scene exceeds 256 nested JSON values".into());
        }
        if entry.nodes > 1_000_000usize.saturating_sub(self.nodes) {
            return Err("scene exceeds one million decoded values".into());
        }
        if entry.text_bytes > (64 * 1024 * 1024usize).saturating_sub(self.bytes) {
            return Err("scene strings exceed 64 MiB".into());
        }
        *self.depths.last_mut().unwrap() = depth + entry.height;
        self.nodes += entry.nodes;
        self.bytes += entry.text_bytes;
        pass.stats.hits += 1;
        pass.stats.skipped_nodes += entry.nodes;
        pass.insert(key, entry.clone());
        Ok(Some(entry.value.clone()))
    }
    pub(super) fn retain(
        &mut self,
        value: ValueHandle,
        decoded: &Value,
        start: (usize, usize),
        height: usize,
    ) -> Result<(), String> {
        let Some(pass) = self.cache.as_mut() else {
            return Ok(());
        };
        // Start with leaf scene objects. Ancestor copies would repeatedly own the
        // same native subtree and scan it again for receipts during cold export.
        if decoded.get("kind").is_none()
            || decoded.get("props").is_none()
            || !decoded
                .get("children")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            || pass.budget.steps == 0
            || pass.next.entries.len() >= MAX_ENTRIES
        {
            return Ok(());
        }
        let key = self
            .session
            .data_observation_key(value)
            .map_err(|e| format!("{e:?}"))?;
        if pass.next.entries.contains_key(&key) {
            return Ok(());
        }
        let nodes = self.nodes - start.0;
        let text_bytes = self.bytes - start.1;
        // Conservative native JSON/map/string storage, plus hash table overhead.
        let native_bytes = nodes
            .saturating_mul(128)
            .saturating_add(text_bytes.saturating_mul(2))
            .saturating_add(256);
        if native_bytes > MAX_BYTES.saturating_sub(pass.next.bytes) {
            return Ok(());
        }
        pass.stats.attempts += 1;
        let Some(receipt) = self
            .session
            .observe_data(value, &mut pass.budget)
            .map_err(|e| format!("{e:?}"))?
        else {
            return Ok(());
        };
        let bytes = native_bytes.saturating_add(receipt.bytes());
        if bytes > MAX_BYTES.saturating_sub(pass.next.bytes) {
            return Ok(());
        }
        pass.stats.admitted += 1;
        pass.insert(
            key,
            Arc::new(Entry {
                receipt,
                value: decoded.clone(),
                nodes,
                text_bytes,
                height,
                bytes,
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot_core::GlobalName;
    use knot_workbench::{libraries::Libraries, project::ProjectSnapshot};
    use std::path::Path;

    #[test]
    fn cached_decoding_preserves_export_limits_and_budget_misses_decode_normally() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR"));
        let snapshot = ProjectSnapshot::load(
            &app.join("tests/fixtures/demo.knot"), &[app.join("..")], &Default::default(),
            Some("module Main exposing (..)\nimport Scene\nexported = Scene.JsonObject [(\"kind\", Scene.JsonString \"circle\"), (\"props\", Scene.JsonObject [(\"id\", Scene.JsonString \"hello\")]), (\"children\", Scene.JsonArray [])]\n".into()),
            &Default::default(),
        ).unwrap();
        let modules = snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .collect::<Vec<_>>();
        let program = knot_core::link::link_borrowed(
            Libraries::load(None).unwrap().0.core,
            &modules,
            &Default::default(),
        )
        .unwrap();
        let mut session = twine::Session::new(Arc::new(program), twine::Limits::default());
        let root = session
            .evaluate(&GlobalName {
                module: vec!["Main".into()],
                name: "exported".into(),
            })
            .unwrap();
        let (expected, cache, stats) = decode_cached(session.read_values(), root, None).unwrap();
        assert_eq!(stats.admitted, 1);
        let (actual, _, stats) = decode_cached(session.read_values(), root, Some(&cache)).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(stats.hits, 1);
        let entry = cache.entries.values().next().unwrap();
        for (nodes, bytes, depth, error) in [
            (1_000_001 - entry.nodes, 0, 0, "one million"),
            (0, 64 * 1024 * 1024 + 1 - entry.text_bytes, 0, "64 MiB"),
            (0, 0, 257 - entry.height, "256 nested"),
        ] {
            let mut decoder = Decoder {
                session: session.read_values(),
                nodes,
                bytes,
                depths: Vec::new(),
                cache: Some(Pass {
                    previous: Some(&cache),
                    next: DecodeCache::default(),
                    budget: ObservationBudget {
                        nodes: 4096,
                        steps: 100_000,
                    },
                    stats: DecodeStats::default(),
                }),
            };
            assert!(decoder.value(root, depth).unwrap_err().contains(error));
        }
        let mut decoder = Decoder {
            session: session.read_values(),
            nodes: 0,
            bytes: 0,
            depths: Vec::new(),
            cache: Some(Pass {
                previous: Some(&cache),
                next: DecodeCache::default(),
                budget: ObservationBudget {
                    nodes: 4096,
                    steps: 0,
                },
                stats: DecodeStats::default(),
            }),
        };
        assert_eq!(decoder.value(root, 0).unwrap(), expected);
        assert_eq!(decoder.cache.unwrap().stats.hits, 0);
    }
}
