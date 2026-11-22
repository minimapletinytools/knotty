//! Immutable names used by runtime primitives. These are metadata, not runtime
//! values: every constructed value still gets its ordinary fields and identity.
use knot_core::{prelude, GlobalName};
use std::sync::{Arc, LazyLock};

pub(crate) struct PreludeNames {
    pub true_: Arc<GlobalName>,
    pub false_: Arc<GlobalName>,
    pub lt: Arc<GlobalName>,
    pub eq: Arc<GlobalName>,
    pub gt: Arc<GlobalName>,
    pub nil: Arc<GlobalName>,
    pub cons: Arc<GlobalName>,
    pub ratio: Arc<GlobalName>,
    pub nothing: Arc<GlobalName>,
    pub just: Arc<GlobalName>,
}

pub(crate) static PRELUDE: LazyLock<PreludeNames> = LazyLock::new(|| PreludeNames {
    true_: Arc::new(prelude("True")),
    false_: Arc::new(prelude("False")),
    lt: Arc::new(prelude("LT")),
    eq: Arc::new(prelude("EQ")),
    gt: Arc::new(prelude("GT")),
    nil: Arc::new(prelude("[]")),
    cons: Arc::new(prelude(":")),
    ratio: Arc::new(prelude("Ratio")),
    nothing: Arc::new(prelude("Nothing")),
    just: Arc::new(prelude("Just")),
});
