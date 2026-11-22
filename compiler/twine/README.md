# Twine runtime

Twine executes the checker output through `knot-core`, with a runnable v0 evaluator
and an initial bounded inspection API. The supported path includes Int/Integer/Float arithmetic, exact
decimal input, lazy functions,
recursive lets, explicit instance dictionaries, basic constructors/patterns,
records/lists/tuples, do blocks, derived and structural dictionaries, Unicode
text primitives, Map/String library execution and basic IO.

Rust embedders can also register nominal opaque types and pure host functions.
The [host type guide](host-types.md) describes the checker/runtime contract,
ownership, cooperative budgets and a runnable Mesh example. Embedders can opt into
revision identities and bounded demand tracking;
this records dependencies but does not yet reuse values across edits.

From the repository root:

```sh
cargo run --manifest-path compiler/Cargo.toml -p knot-cli -- check corpus/run/answer.knot
cargo run --manifest-path compiler/Cargo.toml -p knot-cli -- core corpus/run/answer.knot
cargo run --manifest-path compiler/Cargo.toml -p knot-cli -- eval corpus/run/answer.knot answer
cargo run --manifest-path compiler/Cargo.toml -p knot-cli -- run corpus/run/answer.knot
cargo run --manifest-path compiler/Cargo.toml -p knot-cli -- run corpus/run/greeting.knot
```

`run` requires `tip :: IO Unit` (an equivalent inferred type also works). `eval`
adds and checks a `show` query in the module before type erasure, so dictionary
selection stays in the checker. It rejects functions and other values without
Show evidence. It never executes IO descriptions. The commands load the reachable
local user-module graph plus bundled Prelude, Map and String modules. Imports map
to module-name paths beneath the entry directory or explicit `--module-root DIR`
options. See [local modules](../knot-workbench/modules.md). The library bodies are
lowered through exactly the same path as user bodies; they are not replaced by
runtime type-tag dispatch.

## Core and dictionaries

Core is an expression arena with explicit local/global identities and one source
origin per node. Lambdas bind one argument at a time, preserving pattern demand
under partial application. Let groups are recursive. Pattern lets use one shared
match and lazy projections, preserving whole-pattern demand when a component is
forced. Generated nodes have synthetic origins; primary origins are unique.

Instance dictionaries are lazy records. Explicit methods capture the instance
context; missing methods use the checked interface default applied to the same
recursive dictionary. Superclasses and method-specific dictionary arguments use
the checker's evidence and argument order. Integer literals pass an exact Integer
to `Num.fromInteger`; Int conversion wraps.

Decimal literals call the selected `Fractional.fromRational` with a compact,
exact `Ratio Integer` argument. Its numerator and denominator expand together
only when their Integer values are demanded. A custom instance can inspect or
ignore that argument using ordinary constructor patterns. Float conversion can
round the compact decimal directly without expanding its power of ten, even
through a polymorphic function. The ordinary Ratio instance normalizes through
`%`, so evaluating its result can demand the fields.

An explicit Integer ratio rounds once to binary64, nearest with ties to even,
without first converting its operands to Float. Numerator and denominator are
forced through machine continuations. Finite Floats convert back to their exact
binary ratio; converting infinity/NaN to Integer or Ratio reports `InvalidNumeric`.
An explicit zero denominator reports `DivideByZero`. Float division by zero
retains IEEE behavior. Negation, abs, signum and relational operators preserve
signed-zero/NaN behavior; unordered `compare` returns GT.

Float `show` uses open rounding intervals matching GHC 9.6.2, with exponent form
outside `0.1 <= |x| < 10^7`. Rust's default shortest representation can differ
(for example, `1e23` prints `9.999999999999999e22`). The reproducible GHC fixture
in `tests/fixtures` pins this choice. Transcendental functions use host f64 math;
cross-platform bit-identical results are not promised.

ADTs have generated Eq/Ord/Show methods and, for single-constructor types,
pointwise Semigroup/Monoid methods. Field dictionaries come from the checker's
exported evidence. Comparisons stop at the first unequal field; constructor
ordering follows declaration order. Show uses precedence 10 for constructor
application and 11 for its fields, delegating field rendering to `showsPrec`.
Recursive, mutually recursive, phantom and transformed type arguments are covered.
Generated comparison code grows linearly with the constructor/field count.

Structural Eq/Ord/Show factories are shared by interface and shape (tuple arity,
record labels or unit). Each call supplies its own part dictionaries. Record
labels are sorted; nested structures use their own formatting without extra
parentheses. Ord factories supply Eq through their part dictionaries' superclass
projections. Default methods still use the checked Prelude bodies.

The checked AST keeps do, patterns, lists and annotations intact for Tangle.
Core is a one-way executable product and is not printed back as Knot source.
`knot core` prints a deterministic arena listing, including library code. The
small golden in `corpus/core` omits libraries to keep it reviewable.

## Session API and limits

`Session::new(Arc<Program>, Limits)` owns thunks, values and environment frames.
All recursive edges are integer IDs, so dropping the session releases cyclic
heaps without Rc cycles. The program is trusted compiler-generated core.

- `evaluate(GlobalName)` returns an opaque session-bound `ValueHandle` after
  evaluating to the outer value only. A constrained binding is a dictionary-taking
  function; use a checked query to specialize it.
- `view(handle)` never forces fields or runs IO. It exposes the outer form, not
  a recursive UI inspector. Handles from another session are rejected.
- `with_hosts` links registered functions against checked signatures before
  evaluation. `native_value(handle)` provides non-forcing access to a Rust-owned
  payload for an application renderer; native snapshots are shared through `Arc`.
- `run(GlobalName, IoHandler)` executes an IO description with an explicit
  continuation stack. Repeating it repeats effects, including reads. Host failures
  and EOF are runtime errors; they do not become cached action results.
- `stats`, `activations` and `allocations` expose basic provenance. Allocation is
  not evidence of demand. Activations and value handles are session-local;
  source origins carry checked module revisions and node IDs.

`ReuseLimits.observed_thunks` bounds provenance scanning independently of
`code_nodes` and graph-transfer capacity. Defaults scan 10,000 cells; increasing
`heap_entries` no longer spends the code-matching allowance on scanning.
`ReuseStats` reports scanned cells and transferred thunks, values and environments.

Forcing uses a heap-allocated continuation stack. A thunk transitions from pending
to evaluating to a memoized value or language error. Re-entering an evaluating
thunk reports a loop. A failed thunk returns its original error on later forces.
Errors include their failing source origin and a capped forcing trace.

Limits apply to the session's entire lifetime, including repeated IO runs. Defaults
are fifty million work units, two million heap entries and 16 MiB of literal or
primitive operand/result payload. Work charges include payload size, with a
conservative extra cost for Integer multiplication/division, ratio conversion
and decimal expansion. Decimal expansion also has an absolute exponent cap,
`Limits::decimal_power` (default 4096), checked before allocating a power of ten.
The work/payload limits can reject an expansion below that cap. Compact decimal
Float conversions do not require expansion and are not limited by that exponent
cap. Numeric primitive scratch-space estimates are charged before the operation. These are scheduling
limits, not an exact byte-memory accounting or garbage collector. The heap and
core are released when their owners are dropped; there is no in-session GC yet.
The payload cap bounds individual operations, not aggregate live memory. Work
and heap budgets apply independently and can stop a computation below this cap.
Host callbacks additionally report cumulative native storage against
`Limits::host_bytes` (default 256 MiB), and charge long-running work cooperatively.

Cancellation is checked between machine steps and around bounded primitive work.
Cancellation or budget exhaustion abandons the session; subsequent evaluation and
inspection return `Abandoned`. Dispose it and start another session. Already
performed IO is not undone. The synchronous host must return from blocking IO
before cancellation can be observed. The Rust API accepts a shared cancellation
flag and custom limits. The CLI accepts `--steps`, `--heap-entries`,
`--max-payload-bytes`, `--host-bytes`, `--decimal-power`, and `--stats`.
The stats flag prints evaluator time, work/call counters and heap entries to stderr.
Errors show source excerpts and a bounded demand trace; control-limit failures
include the current work/heap usage and budget options.

Float conversion charges operand-dependent numeric work separately from its
linear scratch allowance. Small conversions no longer cost at least 65,536 work
units. The full SVG drawing takes about 21 million units and 1.21 million heap
entries, fitting the new default work allowance. Float rendering retains its
conservative 1,024-unit allowance; further tuning should use measurements.

Inspection has a separate default 100,000-unit work budget. Interrupted queries
roll back touched thunks and new allocations, preserving the main session;
successful pure values and errors remain memoized. This is retry, not resumable
continuations. See the inspector API for checked expressions,
activation selection, paginated fields and its current precompilation requirement.
`knot inspect FILE BINDING 'head (take 20 value)'` returns WHNF without requiring
Show or executing IO. `--steps` overrides the query allowance too.

Bundled library core is cached on disk using a build-time compiler fingerprint,
bundled source contents and serialization format. Cached revisions are rebased
and user core is appended without recompiling the library bodies. Checksums and
arena validation reject corrupt artifacts; writes are atomic, and cache failures
fall back to compilation. This is a trusted local compiler cache, not a format for
untrusted uploaded programs. It stores no user evaluation results or host values.
Use `--no-cache`, `--cache-dir DIR`, or `KNOT_CACHE_DIR`. The default directory is
`$XDG_CACHE_HOME/knot`, falling back to `$HOME/.cache/knot`. Old compiler-version
artifacts can be deleted freely; automatic disk-cache eviction is not implemented.

## Text and library coverage

All currently declared library primitives have implementations and checked
arities. String rendering follows Haskell, including decimal non-ASCII escapes;
GHC 9.6.2 fixtures cover ASCII controls, Unicode and escape disambiguation.
`String.toList` and `splitOn` build one list cell per demand, with shared UTF-8
storage behind their tails. `concat` forces list heads and tails in order through
machine continuations. Replacement, rendering and case expansion preflight
output size; concat checks accumulated size before appending. Infinite lists
remain subject to cancellation/work limits.

Character predicates use Data.Char category semantics with pinned Unicode 16.0
tables (`unicode-general-category`). Case conversion uses Rust's full per-scalar
mappings, allowing expansion without context-sensitive final-sigma conversion.
`ord` requires one scalar; `chr` rejects surrogates and out-of-range integers.
Empty split separators/replacement patterns are runtime errors.

Text-to-number parsing consumes the entire ASCII decimal string, with optional
sign, no whitespace and no special Float names. Float syntax requires digits on
both sides of a decimal point and after an exponent. Int overflow is Nothing;
valid Float overflow/underflow produces infinity/signed zero. Exact grammars and
rendering policy are in spec §5.7.

Execution tests cover public String operations, Map ordering/updates/deletion,
left-biased union and lazy values, plus an ordered-map oracle for shuffled
insertions/deletions. Representative existing corpus programs also execute.
The workbench runtime sweep covers all 102 supported whole-program fixtures,
with 412 binding/query results including 60 explicit function probes. Show-able
values have reviewed golden output; functions and opaque values also receive
WHNF probes. Prelude probes cover list/traversal operations, numeric defaults,
rounding/sign conventions and lazy control. The complete SVG drawing runs in a
CLI regression test. This is a concrete conformance gate, not an exhaustive proof
of every input. Unknown core primitives report `Unsupported` with an origin.

User-module loading, incremental reevaluation, cross-edit caches, annotation
execution, unravel, live query attachment and full Tangle integration remain
deferred. See archived follow-on plan.

## Validation

```sh
cargo test --manifest-path compiler/Cargo.toml --workspace
cargo clippy --manifest-path compiler/Cargo.toml --workspace --all-targets -- -D warnings
cargo fmt --manifest-path compiler/Cargo.toml --all --check
```

Runtime tests cover laziness, shared thunks, dictionary ABI/defaults, recursion,
pattern demand, error memoization, productive cycles versus loops, cancellation,
resource limits, repeated IO, and deep execution on a 256 KiB native stack.
Numeric tests cover exact/custom/polymorphic decimals, lazy expansion limits,
IEEE signs and comparisons, rounding boundaries, 10,000 exact binary64 round
trips, GHC rendering goldens and origin-bearing conversion errors.
CLI tests run `corpus/run` with captured input/output. Core tests check origins,
linked library references, shared factories, linear generation size and deterministic
printed-core goldens. Set
`UPDATE_KNOT_CORE=1` when intentionally updating that golden.
