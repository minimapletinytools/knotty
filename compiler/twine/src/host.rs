//! Rust embedding boundary. Registered functions must be referentially
//! transparent; native payloads are shared immutable snapshots. Rust callbacks
//! are trusted, cooperative code, not a sandbox or a dynamic-library ABI.
use knot_core::{GlobalName, HostDeclarations, HostSignature, HostType};
use num_bigint::BigInt;
use std::{
    any::{Any, TypeId},
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Message(String),
    BudgetExhausted,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostLinkError {
    MissingFunction(GlobalName),
    SignatureMismatch(GlobalName),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationError(pub String);
/// An embedding-owned immutable input or native snapshot identity. Reusing a
/// token promises identical contents; change it whenever the snapshot changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotId {
    pub namespace: String,
    pub version: String,
}
/// Versions cover callback implementation AND every captured external input.
/// These are explicit opt-in promises, never inferred from function addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostVersion {
    pub implementation: String,
    pub inputs: SnapshotId,
}
#[derive(Clone)]
pub struct NativeValue {
    name: GlobalName,
    payload: Arc<dyn Any + Send + Sync>,
    snapshot: Option<SnapshotId>,
    retained_bytes: usize,
}
impl NativeValue {
    /// Conservative retained-storage estimate charged when introduced or reused.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
    pub fn snapshot_id(&self) -> Option<&SnapshotId> {
        self.snapshot.as_ref()
    }
    pub fn type_name(&self) -> &GlobalName {
        &self.name
    }
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}
impl fmt::Debug for NativeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Native").field(&self.name).finish()
    }
}
#[derive(Debug, Clone)]
pub enum HostValue {
    Int(i64),
    Integer(BigInt),
    Float(f64),
    String(String),
    Bool(bool),
    Unit,
    Native(NativeValue),
}
impl HostValue {
    pub fn native<T: Any>(&self) -> Result<&T, HostError> {
        match self {
            Self::Native(v) => v
                .downcast_ref()
                .ok_or_else(|| HostError::Message("native Rust type mismatch".into())),
            _ => Err(HostError::Message("expected native value".into())),
        }
    }
    pub(crate) fn matches(&self, ty: &HostType, registry: &HostRegistry) -> bool {
        match (self, ty) {
            (Self::Int(_), HostType::Int)
            | (Self::Integer(_), HostType::Integer)
            | (Self::Float(_), HostType::Float)
            | (Self::String(_), HostType::String)
            | (Self::Bool(_), HostType::Bool)
            | (Self::Unit, HostType::Unit) => true,
            (Self::Native(v), HostType::Native(n)) => {
                v.name == *n && registry.types.get(n) == Some(&v.payload.as_ref().type_id())
            }
            _ => false,
        }
    }
}
type Callback =
    dyn Fn(&mut HostContext<'_>, &[HostValue]) -> Result<HostValue, HostError> + Send + Sync;
/// Immutable compatibility descriptor. It owns no callback or external payload.
#[cfg(feature = "incremental")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HostIdentity {
    pub version: HostVersion,
    signature: HostSignature,
    native_types: Vec<(GlobalName, TypeId)>,
    pub bytes: usize,
}
#[cfg(feature = "incremental")]
impl HostIdentity {
    fn new(
        version: HostVersion,
        signature: HostSignature,
        types: &BTreeMap<GlobalName, TypeId>,
    ) -> Self {
        fn name_bytes(name: &GlobalName) -> usize {
            name.module
                .iter()
                .map(String::len)
                .fold(name.name.len(), usize::saturating_add)
        }
        let native_types: Vec<_> = signature
            .parameters
            .iter()
            .chain(std::iter::once(&signature.result))
            .filter_map(|ty| match ty {
                HostType::Native(name) => Some((name.clone(), types[name])),
                _ => None,
            })
            .collect();
        let bytes = [
            std::mem::size_of::<Self>(),
            version.implementation.len(),
            version.inputs.namespace.len(),
            version.inputs.version.len(),
            signature
                .parameters
                .len()
                .saturating_mul(std::mem::size_of::<HostType>()),
            native_types
                .len()
                .saturating_mul(std::mem::size_of::<(GlobalName, TypeId)>()),
        ]
        .into_iter()
        .chain(
            native_types
                .iter()
                .map(|(n, _)| name_bytes(n).saturating_mul(2)),
        )
        .fold(0, usize::saturating_add);
        Self {
            version,
            signature,
            native_types,
            bytes,
        }
    }
}
pub(crate) struct HostFunction {
    pub signature: HostSignature,
    pub call: Arc<Callback>,
    pub version: Option<HostVersion>,
    #[cfg(feature = "incremental")]
    pub identity: Option<Arc<HostIdentity>>,
}
#[derive(Default)]
pub struct HostRegistry {
    declarations: HostDeclarations,
    types: BTreeMap<GlobalName, TypeId>,
    functions: BTreeMap<GlobalName, HostFunction>,
}
impl HostRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn declarations(&self) -> &HostDeclarations {
        &self.declarations
    }
    pub fn register_type<T: Any + Send + Sync>(
        &mut self,
        name: GlobalName,
    ) -> Result<(), RegistrationError> {
        validate_name(&name, true)?;
        if self.types.contains_key(&name) {
            return Err(RegistrationError("duplicate native type".into()));
        }
        self.declarations.types.insert(name.clone());
        self.types.insert(name, TypeId::of::<T>());
        Ok(())
    }
    /// All parameters are forced left-to-right to their outer values before
    /// invocation. Only concrete scalar/native signatures are supported initially.
    pub fn register_function(
        &mut self,
        name: GlobalName,
        signature: HostSignature,
        call: impl Fn(&mut HostContext<'_>, &[HostValue]) -> Result<HostValue, HostError>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), RegistrationError> {
        validate_name(&name, false)?;
        if self.functions.contains_key(&name) {
            return Err(RegistrationError("duplicate host function".into()));
        }
        for ty in signature
            .parameters
            .iter()
            .chain(std::iter::once(&signature.result))
        {
            if let HostType::Native(n) = ty {
                if !self.types.contains_key(n) {
                    return Err(RegistrationError(format!("unregistered native type {n:?}")));
                }
            }
        }
        self.declarations
            .functions
            .insert(name.clone(), signature.clone());
        self.functions.insert(
            name,
            HostFunction {
                signature,
                call: Arc::new(call),
                version: None,
                #[cfg(feature = "incremental")]
                identity: None,
            },
        );
        Ok(())
    }
    /// Register a pure callback whose implementation and captured input snapshot
    /// can be compared across revisions. Existing unversioned registration works
    /// unchanged, but its observations are ineligible for cross-edit reuse.
    pub fn register_versioned_function(
        &mut self,
        name: GlobalName,
        signature: HostSignature,
        version: HostVersion,
        call: impl Fn(&mut HostContext<'_>, &[HostValue]) -> Result<HostValue, HostError>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), RegistrationError> {
        self.register_function(name.clone(), signature, call)?;
        let function = self.functions.get_mut(&name).expect("registered function");
        #[cfg(feature = "incremental")]
        {
            function.identity = Some(Arc::new(HostIdentity::new(
                version.clone(),
                function.signature.clone(),
                &self.types,
            )));
        }
        function.version = Some(version);
        Ok(())
    }
    pub fn versions(&self) -> BTreeMap<GlobalName, Option<HostVersion>> {
        self.functions
            .iter()
            .map(|(n, f)| (n.clone(), f.version.clone()))
            .collect()
    }
    pub(crate) fn compatible_functions(
        &self,
        old: &Self,
    ) -> std::collections::BTreeSet<GlobalName> {
        self.functions
            .iter()
            .filter_map(|(name, new)| {
                let previous = old.functions.get(name)?;
                if new.version.is_none()
                    || new.version != previous.version
                    || new.signature != previous.signature
                {
                    return None;
                }
                let compatible = new
                    .signature
                    .parameters
                    .iter()
                    .chain(std::iter::once(&new.signature.result))
                    .all(|ty| match ty {
                        HostType::Native(name) => {
                            self.types.contains_key(name)
                                && self.types.get(name) == old.types.get(name)
                        }
                        _ => true,
                    });
                compatible.then(|| name.clone())
            })
            .collect()
    }
    pub(crate) fn accepts_snapshot(&self, value: &NativeValue) -> bool {
        value.snapshot.is_some()
            && self.types.get(&value.name) == Some(&value.payload.as_ref().type_id())
    }
    pub(crate) fn function(&self, name: &GlobalName) -> Option<&HostFunction> {
        self.functions.get(name)
    }
}
fn validate_name(name: &GlobalName, is_type: bool) -> Result<(), RegistrationError> {
    let identifier = |s: &str, upper: bool| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| {
            if upper {
                c.is_ascii_uppercase()
            } else {
                c.is_ascii_lowercase()
            }
        }) && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if name.module.is_empty()
        || !name.module.iter().all(|s| identifier(s, true))
        || !identifier(&name.name, is_type)
    {
        return Err(RegistrationError(
            "host names require an ASCII module path and identifier".into(),
        ));
    }
    if name.module.len() == 1 && ["Prelude", "Map", "String"].contains(&name.module[0].as_str()) {
        return Err(RegistrationError(
            "bundled library module names are reserved".into(),
        ));
    }
    Ok(())
}
/// Cooperative session budgets. Long-running callbacks must call charge_work
/// or check_cancelled during their loops, and reserve native storage before
/// allocating it. A callback cannot clear an interruption by catching its error.
pub struct HostContext<'a> {
    registry: &'a HostRegistry,
    cancellation: &'a AtomicBool,
    remaining_work: u64,
    remaining_bytes: usize,
    pub(crate) work_used: u64,
    pub(crate) bytes_used: usize,
    pub(crate) stopped: Option<HostError>,
}
impl<'a> HostContext<'a> {
    pub(crate) fn new(
        registry: &'a HostRegistry,
        cancellation: &'a AtomicBool,
        work: u64,
        bytes: usize,
    ) -> Self {
        Self {
            registry,
            cancellation,
            remaining_work: work,
            remaining_bytes: bytes,
            work_used: 0,
            bytes_used: 0,
            stopped: None,
        }
    }
    fn stop(&mut self, error: HostError) -> HostError {
        self.stopped = Some(error.clone());
        error
    }
    pub fn check_cancelled(&mut self) -> Result<(), HostError> {
        if let Some(error) = &self.stopped {
            return Err(error.clone());
        }
        if self.cancellation.load(Ordering::Relaxed) {
            return Err(self.stop(HostError::Cancelled));
        }
        Ok(())
    }
    pub fn charge_work(&mut self, units: u64) -> Result<(), HostError> {
        self.check_cancelled()?;
        if units > self.remaining_work {
            return Err(self.stop(HostError::BudgetExhausted));
        }
        self.remaining_work -= units;
        self.work_used += units;
        Ok(())
    }
    pub fn reserve_bytes(&mut self, bytes: usize) -> Result<(), HostError> {
        self.check_cancelled()?;
        if bytes > self.remaining_bytes {
            return Err(self.stop(HostError::BudgetExhausted));
        }
        self.remaining_bytes -= bytes;
        self.bytes_used += bytes;
        Ok(())
    }
    /// Reserve estimated retained storage before building an owned native value.
    /// Include buffers owned by T in `bytes`, not just size_of::<T>().
    pub fn native<T: Any + Send + Sync>(
        &mut self,
        name: &GlobalName,
        bytes: usize,
        build: impl FnOnce(&mut Self) -> Result<T, HostError>,
    ) -> Result<HostValue, HostError> {
        self.validate_type::<T>(name)?;
        self.reserve_bytes(bytes)?;
        let payload = Arc::new(build(self)?);
        self.check_cancelled()?;
        Ok(HostValue::Native(NativeValue {
            name: name.clone(),
            payload,
            snapshot: None,
            retained_bytes: bytes,
        }))
    }
    /// Introduce a pre-existing immutable host snapshot. Charge its estimated
    /// retained bytes; repeated introduction is deliberately conservative.
    pub fn native_shared<T: Any + Send + Sync>(
        &mut self,
        name: &GlobalName,
        bytes: usize,
        payload: Arc<T>,
    ) -> Result<HostValue, HostError> {
        self.validate_type::<T>(name)?;
        self.reserve_bytes(bytes)?;
        Ok(HostValue::Native(NativeValue {
            name: name.clone(),
            payload,
            snapshot: None,
            retained_bytes: bytes,
        }))
    }
    /// Introduce an immutable, versioned native snapshot. The host owns the
    /// uniqueness/content contract for (type, namespace, version).
    pub fn native_shared_versioned<T: Any + Send + Sync>(
        &mut self,
        name: &GlobalName,
        bytes: usize,
        payload: Arc<T>,
        snapshot: SnapshotId,
    ) -> Result<HostValue, HostError> {
        let HostValue::Native(mut value) = self.native_shared(name, bytes, payload)? else {
            unreachable!()
        };
        value.snapshot = Some(snapshot);
        Ok(HostValue::Native(value))
    }
    fn validate_type<T: Any>(&self, name: &GlobalName) -> Result<(), HostError> {
        if self.registry.types.get(name) == Some(&TypeId::of::<T>()) {
            Ok(())
        } else {
            Err(HostError::Message(
                "unregistered or mismatched native Rust type".into(),
            ))
        }
    }
}

#[cfg(all(test, feature = "incremental"))]
mod identity_tests {
    use super::*;
    #[test]
    fn host_read_identity_includes_native_registration_even_with_equal_versions() {
        let name = GlobalName {
            module: vec!["Input".into()],
            name: "Native".into(),
        };
        let signature = HostSignature {
            parameters: vec![HostType::Native(name.clone())],
            result: HostType::Int,
        };
        let version = HostVersion {
            implementation: "same".into(),
            inputs: SnapshotId {
                namespace: "input".into(),
                version: "1".into(),
            },
        };
        let a = HostIdentity::new(
            version.clone(),
            signature.clone(),
            &BTreeMap::from([(name.clone(), TypeId::of::<i64>())]),
        );
        let b = HostIdentity::new(
            version,
            signature,
            &BTreeMap::from([(name, TypeId::of::<u64>())]),
        );
        assert_ne!(a, b);
    }
}
