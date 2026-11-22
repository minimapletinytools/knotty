//! Opt-in diagnostics. Forced-data mode changes demand order, never the default renderer.
use knot_workbench::operation::OperationSession;
use std::{collections::HashSet, time::Instant};
use twine::{Stats, ThunkHandle, ValueHandle, ValueReader, ValueView};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Off,
    Phases,
    ForceData,
    DecodeReplay,
    Layers,
}
impl Mode {
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("KNOT_RENDER_PROFILE").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("off") => Ok(Self::Off),
            Ok("phases") => Ok(Self::Phases),
            Ok("layers") => Ok(Self::Layers),
            Ok("decode-replay") => Ok(Self::DecodeReplay),
            Ok("force-data") => Ok(Self::ForceData),
            _ => Err(
                "KNOT_RENDER_PROFILE must be off, phases, force-data, decode-replay or layers"
                    .into(),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Work {
    pub steps: u64,
    pub thunk_evaluations: u64,
    pub activations: u64,
    pub primitive_calls: u64,
    pub memo_hits: u64,
    pub application_cache_hits: u64,
    pub expression_cache_hits: u64,
    pub captured_cache_hits: u64,
    pub heap_entries: i64,
}
impl Work {
    pub fn read(session: &OperationSession) -> Self {
        let Stats {
            steps,
            thunk_evaluations,
            activations,
            primitive_calls,
            memo_hits,
            application_cache_hits,
            expression_cache_hits,
            captured_cache_hits,
            ..
        } = session.stats();
        Self {
            steps,
            thunk_evaluations,
            activations,
            primitive_calls,
            memo_hits,
            application_cache_hits,
            expression_cache_hits,
            captured_cache_hits,
            heap_entries: session.heap_entries() as i64,
        }
    }
    fn difference(self, before: Self) -> Self {
        Self {
            steps: self.steps - before.steps,
            thunk_evaluations: self.thunk_evaluations - before.thunk_evaluations,
            activations: self.activations - before.activations,
            primitive_calls: self.primitive_calls - before.primitive_calls,
            memo_hits: self.memo_hits - before.memo_hits,
            application_cache_hits: self.application_cache_hits - before.application_cache_hits,
            expression_cache_hits: self.expression_cache_hits - before.expression_cache_hits,
            captured_cache_hits: self.captured_cache_hits - before.captured_cache_hits,
            heap_entries: self.heap_entries - before.heap_entries,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Phase {
    pub ms: f64,
    pub work: Work,
}
/// One immediate child of the source drawing container, including its descendants.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Layer {
    pub kind: String,
    pub children: usize,
    pub ms: f64,
    pub thunks: u64,
    pub primitives: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Layers {
    pub first: Vec<Layer>,
    pub replay: Vec<Layer>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Profile {
    pub force_data: bool,
    /// Cumulative work already charged before output demands, including transfer.
    pub before_outputs: Work,
    pub data: Option<Phase>,
    pub data_replay: Option<Phase>,
    pub data_values: usize,
    pub root: Option<Phase>,
    pub svg: Option<Phase>,
    pub scene_root: Option<Phase>,
    pub scene_force: Option<Phase>,
    pub scene_replay: Option<Phase>,
    pub scene_values: usize,
    pub scene_decode: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_replay: Option<Phase>,
    pub native_svg: Option<Phase>,
    pub after_outputs: Work,
}

pub fn measure<T>(
    session: &mut OperationSession,
    enabled: bool,
    run: impl FnOnce(&mut OperationSession) -> Result<T, String>,
) -> Result<(T, Option<Phase>), String> {
    let start = enabled.then(|| (Work::read(session), Instant::now()));
    let result = run(session)?;
    let phase = start.map(|(before, started)| finish(session, before, started));
    Ok((result, phase))
}

/// Demand all reachable finite data fields, without allocating a native scene.
/// Handle identity avoids revisiting shared data; cycles/functions reject. Bounds
/// and ordinary runtime limits prevent profiling from walking unbounded output.
pub fn force_data(mut reader: ValueReader<'_>, root: ValueHandle) -> Result<usize, String> {
    enum Item {
        Value(ValueHandle),
        Force(ThunkHandle),
        Leave(ValueHandle),
    }
    let mut pending = vec![Item::Value(root)];
    let mut active = HashSet::new();
    let mut complete = HashSet::new();
    let mut visits = 0usize;
    while let Some(item) = pending.pop() {
        visits += 1;
        if visits > 1_000_000 {
            return Err("render profile exceeded one million graph visits".into());
        }
        let value = match item {
            Item::Force(handle) => reader.force(handle).map_err(|e| format!("{e:?}"))?,
            Item::Value(value) => value,
            Item::Leave(value) => {
                active.remove(&value);
                complete.insert(value);
                continue;
            }
        };
        if complete.contains(&value) {
            continue;
        }
        if !active.insert(value) {
            return Err("render profile cannot force cyclic data".into());
        }
        pending.push(Item::Leave(value));
        let fields: Vec<_> = match reader.view(value).map_err(|e| format!("{e:?}"))? {
            ValueView::Literal(_) => vec![],
            ValueView::Constructor(_, _) | ValueView::Tuple(_) => reader
                .indexed_fields(value)
                .map_err(|e| format!("{e:?}"))?
                .unwrap()
                .iter()
                .collect(),
            ValueView::Record(_) => reader
                .record_fields(value)
                .map_err(|e| format!("{e:?}"))?
                .unwrap()
                .iter()
                .map(|(_, h)| h)
                .collect(),
            _ => return Err("render profile requires finite ordinary data".into()),
        };
        pending.extend(fields.into_iter().rev().map(Item::Force));
    }
    Ok(complete.len())
}

pub fn finish(session: &OperationSession, before: Work, started: Instant) -> Phase {
    Phase {
        ms: started.elapsed().as_secs_f64() * 1000.0,
        work: Work::read(session).difference(before),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot_checker::host::HostDeclarations;
    use knot_workbench::{libraries::Libraries, project::ProjectSnapshot};

    #[test]
    fn full_data_walk_preserves_sharing_and_rejects_cycles_and_functions() {
        let dir = std::env::temp_dir().join(format!("knot-data-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Main.knot");
        let libraries = Libraries::bundled(None).unwrap().core;
        for (source, expected) in [
            ("shared :: List Int\nshared = [1,2,3]\nprogram :: (List Int, List Int)\nprogram = (shared, shared)\n", None),
            ("program :: List Int\nprogram = 1 : program\n", Some("cyclic")),
            ("program :: Int -> Int\nprogram x = x\n", Some("ordinary data")),
        ] {
            std::fs::write(&path, format!("module Main exposing (..)\n{source}")).unwrap();
            let snapshot = ProjectSnapshot::load(&path, &[], &HostDeclarations::default(), None, &Default::default()).unwrap();
            let prepared = snapshot.prepare_operations(libraries.clone(), &HostDeclarations::default(), &["program"]).unwrap();
            let mut session = prepared.start(Default::default()).unwrap();
            let binding = session.binding("program").unwrap();
            let root = session.force(&binding).unwrap();
            let first = force_data(session.read_values(), root);
            if let Some(message) = expected {
                assert!(first.unwrap_err().contains(message));
            } else {
                let count = first.unwrap();
                assert!(count > 3);
                let before = session.stats();
                assert_eq!(force_data(session.read_values(), root).unwrap(), count);
                assert_eq!(session.stats().thunk_evaluations, before.thunk_evaluations);
                assert_eq!(session.stats().primitive_calls, before.primitive_calls);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
