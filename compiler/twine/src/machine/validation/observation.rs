use super::*;

impl Session {
    /// Optional validation uses the ordinary current-world verdict table and
    /// code/input rules. Exhausting its smaller budget is a miss, not an
    /// abandoned evaluation. Work still consumes the session's total budget.
    pub(in crate::machine) fn validate_observation_proof(
        &mut self,
        witness: &evidence::Witness,
        limits: &Limits,
    ) -> Result<bool, Outcome> {
        if !self.evidence.owns_witness(witness) {
            return Ok(false);
        }
        self.evidence.validate_with_code(
            witness.proof,
            &self.hosts,
            &mut self.validation.verdicts,
            limits,
            &mut self.stats.steps,
            |recipe| {
                if self
                    .source_code
                    .as_ref()
                    .is_none_or(|view| view.accepts(recipe, &self.values))
                {
                    evidence::CodeValidity::CheckReads
                } else {
                    evidence::CodeValidity::Changed
                }
            },
        )
    }
}
