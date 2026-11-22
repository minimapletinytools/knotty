//! Completed expansion of a shared global binding within one routing wave.
//! Code, policy registrations, candidate values and solver pins cannot change
//! during a wave. Entries are never reused across validation, repair or retry.
use super::*;
use std::collections::HashMap;

#[derive(PartialEq, Eq, Hash)]
pub(super) struct Key {
    binding: GlobalName,
    value_type: String,
    request_type: String,
    locked: bool,
    request: twine::ObservedDataKey,
}
#[derive(Default)]
pub(super) struct UnravelCache {
    keys: HashMap<Key, usize>,
    completed: Vec<bool>,
    bytes: usize,
    pub hits: usize,
}
impl UnravelCache {
    pub fn clear_wave(&mut self) {
        self.keys.clear();
        self.completed.clear();
        self.bytes = 0;
    }
    pub fn complete(&mut self, entries: Vec<usize>) {
        for id in entries {
            self.completed[id] = true;
        }
    }
}
impl Walker<'_> {
    /// Only entry globals without a lexical capture/recipe are eligible. The
    /// request key is full bounded data already demanded by fits; obtaining
    /// it never forces extra fields or uses user-defined equality.
    pub(super) fn shared_global(
        &mut self,
        site: &Site,
        binding: &GlobalName,
        request: &TypedValue,
        ty: &Ty,
        completions: &mut Vec<usize>,
    ) -> Result<bool, EditError> {
        // Patched occurrences can cease to denote their original global. Until
        // we have a candidate-aware source witness, only route the original AST.
        if self.has_candidate_values()
            || site.synthetic
            || site.candidate_source
            || site.recipe.is_some()
            || !site.projections.is_empty()
            || site.fixed
        {
            return Ok(false);
        }
        let Some(value_type) = &site.ty else {
            return Ok(false);
        };
        let Some(request) = self.probes.observed_data_key(request)? else {
            return Ok(false);
        };
        let key = Key {
            binding: binding.clone(),
            value_type: value_type.source(),
            request_type: ty.source(),
            locked: site.locked,
            request,
        };
        if let Some(id) = self.unravel_cache.keys.get(&key) {
            if self.unravel_cache.completed[*id] {
                self.unravel_cache.hits += 1;
                return Ok(true);
            }
            // Recursion through an unfinished entry must still execute and hit
            // the normal traversal bound. It is not a successful fixed point.
            return Ok(false);
        }
        let bytes = key.binding.module.iter().map(String::len).sum::<usize>()
            + key.binding.name.len()
            + key.value_type.len()
            + key.request_type.len()
            + key.request.byte_len();
        if self.unravel_cache.keys.len() >= self.probes.limits.traversal.retained_values
            || self.unravel_cache.bytes.saturating_add(bytes) > self.probes.limits.source_bytes
        {
            return Ok(false);
        }
        self.unravel_cache.bytes += bytes;
        let id = self.unravel_cache.completed.len();
        self.unravel_cache.completed.push(false);
        self.unravel_cache.keys.insert(key, id);
        completions.push(id);
        Ok(false)
    }
}
