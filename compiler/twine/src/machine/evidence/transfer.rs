//! Transfer validity without retaining historical values, closures or recipes.
//! All code edges need exact receiving correspondence; missing children remain
//! unknown. The caller has already validated the root for the receiving world.
use super::*;

impl Store {
    pub(in crate::machine) fn import_proof(
        &mut self,
        source: &Store,
        root: Proof,
        imported: &mut HashMap<storage::Id, Proof>,
        budget: (&Limits, &mut u64),
        code: impl Fn(&Recipe) -> Option<SummaryCode>,
    ) -> Proof {
        let (limits, work) = budget;
        let Some(root_id) = root.id else {
            return Proof::default();
        };
        let bound = limits.dependency_records.min(limits.dependency_bytes / 128);
        if bound == 0 {
            return Proof::default();
        }
        let mut stack = vec![(root_id, 0usize)];
        let mut visiting = std::collections::HashSet::new();
        while let Some((id, next)) = stack.last_mut() {
            if limits.cancellation.load(Ordering::Relaxed) || !charge(work, limits) {
                return Proof::default();
            }
            if imported.contains_key(id) {
                visiting.remove(id);
                stack.pop();
                continue;
            }
            if imported.len() >= bound {
                return Proof::default();
            }
            let Some(record) = source.records.get(*id) else {
                return Proof::default();
            };
            if *next == 0 && !visiting.insert(*id) {
                return Proof::default();
            }
            if let Some(child) = record.reads.get(*next) {
                *next += 1;
                let Some(child) = child.id else {
                    return Proof::default();
                };
                if stack.len() >= bound {
                    return Proof::default();
                }
                stack.push((child, 0));
                continue;
            }
            let copy_work = (record.reads.len() as u64)
                .saturating_mul(2)
                .saturating_add(1)
                .saturating_add(match &record.recipe {
                    Recipe::Host(name, _) => {
                        name.module
                            .iter()
                            .map(String::len)
                            .sum::<usize>()
                            .saturating_add(name.name.len()) as u64
                    }
                    _ => 0,
                });
            if copy_work > limits.steps.saturating_sub(*work) {
                return Proof::default();
            }
            *work += copy_work;
            let complete = record.reads.iter().all(|read| {
                read.id
                    .and_then(|id| imported.get(&id))
                    .is_some_and(|read| read.id.is_some())
            });
            let expression = if matches!(record.recipe, Recipe::Host(..)) {
                Some(SummaryCode::Immutable)
            } else {
                code(&record.recipe)
            };
            let bytes = BASE_BYTES
                .saturating_add(record.reads.len().saturating_mul(READ_BYTES))
                .saturating_add(match &record.recipe {
                    Recipe::Host(name, identity) => identity
                        .bytes
                        .saturating_add(name.module.iter().map(String::len).sum::<usize>())
                        .saturating_add(name.name.len()),
                    _ => 0,
                });
            let proof = match expression {
                Some(expression) if complete && self.room(record.reads.len(), bytes, limits) => {
                    let recipe = match &record.recipe {
                        Recipe::Host(name, identity) => {
                            Recipe::Host(name.clone(), identity.clone())
                        }
                        _ => Recipe::Historical(expression.expression()),
                    };
                    let reads = record
                        .reads
                        .iter()
                        .map(|read| imported[&read.id.unwrap()])
                        .collect();
                    if matches!(record.recipe, Recipe::Summary(_)) {
                        self.insert_summary(expression, reads, limits, work)
                    } else {
                        self.insert_record(recipe, None, reads, record.cost, bytes, limits)
                    }
                }
                _ => {
                    self.stats.unrecorded = self.stats.unrecorded.saturating_add(1);
                    Proof::default()
                }
            };
            imported.insert(*id, proof);
            visiting.remove(id);
            stack.pop();
        }
        imported.get(&root_id).copied().unwrap_or_default()
    }
}
