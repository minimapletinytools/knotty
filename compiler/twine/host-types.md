# Embedding Rust types in Knot

An application can register a Rust-owned type such as `Geometry.Mesh`, then
expose pure functions that consume and return it. Knot sees a nominal opaque
type with no constructors or fields. Rust keeps its efficient representation;
passing a Mesh through Knot clones an `Arc`, not its vertex buffers.

The initial embedding API is implemented. Run the complete example from the
repository root:

```sh
cargo run --manifest-path compiler/Cargo.toml -p twine --example native_mesh
```

It registers `Mesh`, `line`, `scale` and `vertexCount`, runs this Knot program,
and reads the resulting vertices back in Rust:

```haskell
import Geometry
mesh = Geometry.scale 3 (Geometry.line 2)
tip = print (Geometry.vertexCount mesh)
```

## Registration and checking

1. Create a `HostRegistry` and register each qualified type with
   `register_type::<Mesh>(name)`.
2. Register functions with a `HostSignature` and a Rust callback. A signature
   lists parameters in application order and one result. Supported boundary
   types are `Int`, `Integer`, `Float`, `String`, `Bool`, `Unit` and registered
   native types. An empty parameter list describes a demand-driven value.
3. Pass `registry.declarations()` to canonicalization as its `ModuleRegistry`,
   then to `check_program_with_hosts`. This provides both import exports and
   types; ordinary `check_program` does not know application libraries.
4. Lower the checked user program and required bundled libraries as usual.
5. Construct `Session::with_hosts(program, limits, Arc::new(registry))`.

The typed program owns the declarations, with no Rust callbacks or payloads.
Core retains qualified host references and their signatures. Session linking
rejects missing functions and changed signatures before any evaluation,
including references in unused branches. Runtime argument and result checks
also verify the nominal name and underlying Rust `TypeId`.

Native types have kind `Type`; `Mesh Int` is rejected. Two qualified type names
remain distinct even if both use the same Rust representation. Bundled module
names are reserved. Registration currently accepts ASCII module paths and names,
not symbolic operators. This registration mechanism does not implement general
user-module loading.

## Evaluation and ownership

Host applications are lazy until demanded. Partial applications do not force
arguments; a saturated call forces all parameters left-to-right to their outer
values before invoking Rust. Pure calls participate in ordinary thunk sharing
and error memoization. This is per-thunk sharing, not global memoization of
every call with equal arguments.

Callbacks must be referentially transparent and payloads must be observably
immutable snapshots. For example, `scale` returns a new Mesh without changing
the original. Internal caches are acceptable only if they cannot change Knot's
observable results. Rust interior mutability does not enforce this contract.
File reads, clocks and mutations belong in a future explicit host IO interface;
they must not be hidden inside these pure callbacks. Application inputs can be
captured as immutable snapshots when constructing the registry.

Use `HostContext::native(name, estimated_bytes, build)` to reserve storage
before constructing a value, or `native_shared` to introduce an existing `Arc`.
Include owned buffers in the estimate. `HostValue::native::<Mesh>()` provides
read-only access to an argument. Callbacks can return an existing native argument
cheaply. If a callback captures a retained `NativeValue` from another session,
it must explicitly reserve its retained storage before returning it; returning
that clone does not automatically charge its bytes in the new session.

The session and any explicitly retained native clones share ownership. Dropping
the last owner releases the Rust payload. `Session::native_value(handle)` exposes
an already evaluated payload for an embedding renderer without forcing more
Knot code. Its `NativeValue` can be cloned to outlive the session. Ordinary
`ValueHandle`s remain session-bound and cannot be used in another session.

Knot can put native values in its own lists, records, tuples and ADTs, pass them
through polymorphic Knot functions, and define explicit instances for them.
There is no automatic `Eq`, `Ord` or `Show` for opaque types. For example, a
`Show Mesh` instance can call a registered summary function; no raw Rust layout
or pointer identity becomes language-level equality.

## Errors and resource limits

`HostError::Message` becomes a memoized runtime error with a source origin.
Mismatched return types and unwinding callback panics also become runtime errors.
This does not catch aborting panics or process faults. Recoverable domain errors
represented by Knot ADTs need a richer boundary API in a later slice.

`charge_work` consumes session work units; `reserve_bytes` consumes a cumulative
host-storage budget (`Limits::host_bytes`, default 256 MiB). Long operations must
charge work or check cancellation within their loops. A callback cannot swallow
an interruption and make the session usable again. Cancellation or budget
exhaustion abandons the session, just as it does in the evaluator. Scalar
operands/results also obey the existing primitive payload limit.

These are trusted, cooperative Rust callbacks. Reported storage is an estimate,
not measured live memory; reservations are not refunded. Reserve before large
allocations, including scalar outputs and temporary buffers when appropriate.
Twine cannot interrupt a blocking callback or enforce honest cost reporting.
This API is neither a sandbox nor a stable dynamic-library ABI.

## Follow-up boundaries

The first slice does not include parameterized native types, polymorphic host
signatures, list/record/ADT conversion at the Rust boundary, callbacks into Knot,
custom host IO actions, or serialization. These can be added without exposing
Mesh internals to Knot.

The bounded inspector exposes opaque native types and non-forcing native access
for application renderers. Prepared queries can call registered pure summary
functions, with activation selection and transactional work/storage budgets.
See the inspection API. UI recursive previews and application
renderers still need their own presentation bounds and must never run IO.

`register_versioned_function` accepts a `HostVersion` with an implementation token
and captured-input `SnapshotId`; `native_shared_versioned` attaches a snapshot ID
to an immutable payload. Equal `(type, namespace, version)` promises equal contents.
Change implementation tokens for callback semantics or native representation/ABI
changes, and input tokens when captured inputs change. Unversioned registration
remains supported but blocks cross-edit reuse through those calls. See revision tracking
for dependency tracking, and bounded evaluation reuse for the
implemented cross-edit graph transfer. Native snapshots also require compatible
Rust type registrations and charge their retained-byte estimate in the receiving
session; qualified names/signatures alone never establish validity.
