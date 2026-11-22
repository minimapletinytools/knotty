//! Call-by-need evaluation with a session-owned heap and explicit continuations.
//! IO construction is pure; only `Session::run` invokes a host handler.
mod host;
mod machine;
pub use host::{
    HostContext, HostError, HostLinkError, HostRegistry, HostValue, HostVersion, NativeValue,
    RegistrationError, SnapshotId,
};
pub use knot_core::{HostDeclarations, HostSignature, HostType};
#[cfg(feature = "cost-accounting")]
pub use machine::WorkCost;
#[cfg(feature = "incremental")]
pub use machine::{
    CacheIndexStats, CacheRetention, CollectionBudget, DataObservation, DataObservationKey,
    DependencyRead, DependencySource, EvidenceCollection, EvidenceCollectionStatus,
    EvidencePressure, EvidenceStats, ForkAttempt, InputForkAttempt, ObservationBudget,
    RetainedRoot, Revision, RevisionCollection, RevisionCollectionStatus, RevisionContext,
    RevisionError, RevisionForkAttempt, RevisionLimits, RevisionPreparation, RevisionReuse,
    RevisionStore, SessionFork, SnapshotInheritance, SourceForkAttempt, SourceForkLimits,
    ValidationStats,
};
mod numeric;
mod primitive;
mod text;
pub use machine::{
    Activation, ActivationId, Allocation, BindingView, Child, ExtensionError, IndexedFields,
    Inspection, InspectionBudget, Limits, ObservedDataKey, Outcome, RecordFields, ReuseLimits,
    ReuseStats, RuntimeError, RuntimeErrorKind, Session, Stats, ThunkHandle, ValueHandle,
    ValueReader, ValueView,
};

pub trait IoHandler {
    fn put_line(&mut self, text: &str) -> Result<(), String>;
    /// None is end of input; an empty line is Some("").
    fn get_line(&mut self) -> Result<Option<String>, String>;
}
#[derive(Debug, Default)]
pub struct CapturedIo {
    pub input: std::collections::VecDeque<String>,
    pub output: Vec<String>,
}
impl IoHandler for CapturedIo {
    fn put_line(&mut self, text: &str) -> Result<(), String> {
        self.output.push(text.into());
        Ok(())
    }
    fn get_line(&mut self) -> Result<Option<String>, String> {
        Ok(self.input.pop_front())
    }
}

pub use machine::dependencies::*;
