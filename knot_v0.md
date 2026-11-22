# Knot v0: scope and implementation status

Knot is the language, Tangle is its visual editor, and Twine is its evaluator.
The language design is in [language-spec-notes.md](language-spec-notes.md).
This file records the supported language and runtime. The
[unravel design](unravel-design.md) and [migration notes](unravel-design-followups.md)
cover reverse editing. The compiler loads the [bundled Prelude](compiler/knot-canonical/lib/Prelude.knot).

## Deliberate v0 restrictions

### Instances and types

- One-parameter instance heads must be nominal constructors applied to distinct
  type variables.
  `Eq Shape`, `Eq (Box a)` and `Functor (Outcome e)` are supported. Specialized
  heads (`Eq (Box Int)`), nested arguments (`Eq (Box (List a))`), repeated variables
  (`Eq (Pair a a)`), and context variables absent from the head are rejected.
  General head matching/specialization for these interfaces remains deferred.
- Two-parameter `Constriction s t` is implemented through inference, dictionaries,
  imports and Twine execution. Exact, optional-exact and list-alternative
  instances are in the Prelude. Compiler-owned fieldwise validation supports
  equal-arity tuples and closed records with matching field names; identical
  types retain exact equality. Omitted-field and open-row constraints are deferred.
  User relation patterns support variables and
  nominal applications in both positions, with overlap and termination checks.
  No functional dependencies, instance-directed inference or implicit conversions.
- Custom record, tuple and unit instances are rejected, including through aliases.
  Automatic structural `Eq`, `Ord` and `Show` remain available and cannot be
  overridden. Use a single-constructor ADT to own custom behavior; dedicated
  `newtype` syntax is not implemented.
- Interfaces are a closed prelude-defined set. Users may add instances, not new
  interfaces. Deriving is limited to `Eq`, `Ord`, `Show`, and single-constructor
  `Semigroup`/`Monoid`, with the field restrictions in the language spec.
- Unresolved unary constraints on compound types (`Eq (f a)`) and structural constraints on open
  rows cannot be generalized. Open-record field access and record patterns work;
  equality/showing a record must wait until its row closes. There is no flexible
  context or row-constraint language yet. Signature contexts still use bare type
  variables; inferred binary predicates may retain compound arguments.
- Integer literal patterns are `Int` patterns only. `Integer` and custom numeric
  pattern matching via `fromInteger`/`==` are deferred; use explicit comparisons.
  Float literal patterns are unsupported.
- Pattern `let` bindings and recursive groups containing them are monomorphic.
- Tuples have fixed arity with no language-level maximum; variadic tuple
  polymorphism and spreading are unsupported. Unit-aware numeric types are future work.

### Annotations, reverse editing and graph tooling

- The front end parses annotations, resolves names and retains metadata. General
  annotation value checking is deferred. The source editor
  checks selected `@unravel` rules. Direct request-type matches, including
  polymorphic specializations, follow attachment order. Failure of the selected rule is final.
- Functions, constructors, tuples and records use the same feed/request contract.
  Hosts can attach constructor and product rules.
  Source attachment syntax for constructors is pending. The editor follows
  source projections, retains pins and branch
  decisions, and validates the edited program before publishing a revision-safe patch.
- At a [creation or replacement boundary](compiler/knot-workbench/candidate-constructions.md),
  the request's `Constriction.materialize` supplies a candidate. The engine checks
  `fits` and constructs the source edit. List growth preserves existing element
  expressions. Shared-input solving
  supports ordered request batches and named value locks within shared budgets.
  General partial-pin repair remains deferred.
- Generated callback companions support unary
  and multiargument callbacks, with a `Maybe` request and an `Eq` requirement for
  each ordinary input. Author-written companions may choose other constraints.
  Prelude `map` preserves length and `growMap` opts into seeded growth. Editing
  already-supplied callback arguments and callbacks that themselves accept functions are deferred.
- The design specifies `Maybe T` as the default sensitivity for primitives and
  `ListConstraint` for lists. The bundled Prelude still declares `Sensitivity`
  as opaque. Automatic derivation and `ToSensitivity` / `FromSensitivity` dispatch
  remain pending. The source editor supports explicit one-step request adapters.
- The [drawing app](README.md#run-locally) implements `@whip` sliders
  for top-level `Int` bindings. A slider can appear drawing-wide or on selection
  of a referenced output node. The app handles placement and Undo.
- Process-local cross-edit identities and opt-in demand tracking are available
  through the revision API. Conservative
  pure-graph reuse can seed a new session with
  structures, lazy fields, closures, dictionaries and compatible applications.
  Versioned host calls and immutable native snapshots can opt in. Compatibility
  uses code, captures, dictionaries and dependency identities; equal results alone
  are not cache keys. Persistent document IDs across process restarts remain separate work.
- All source queries and unravel calls share one typed transaction runtime.
  The separate decoded-result replay cache is removed. Runtime reuse charges
  actual work; checked-code caches remain separate. The source editor does not
  automatically retain previous transaction heaps. Typed operation hosts can
  opt into the same bounded graph transfer used by the renderer.
- Tangle is a component prototype, not an integrated source editor/runtime.
  Source printing, source/graph round-trip tests, comment preservation, incomplete
  graph persistence, execution ordering UI, and application host contracts remain
  to be implemented or specified. Retaining `do` is implemented in the surface,
  canonical and typed ASTs, including do-let statements. Twine lowers these for
  execution; the UI integration is still missing.

### Other omitted syntax

No guards, multi-equation functions, list comprehensions, map literals, operator
sections such as `(+2)`, range syntax, explicit-brace layout blocks, local operator
definitions, user constructor operators, `Char`, or `.`, `$`, `<|`, `<<`, `=<<`
operators. Record patterns are shorthand only. Named expression holes are not
supported; `_` expression holes are reported as errors with inferred types.
See the language spec for the supported alternatives.

## Checker status and remaining work

`check_program` and `check_library_program` now export an owned `TypedProgram`:
typed function/instance/default bodies, top-level and local schemes, substitutions,
module/node/binding identities and ordered per-node dictionary expressions.
Generalized obligations become lexical dictionary parameters; unresolved evidence
is rejected. This output includes literals, recursive references and implicit
do binds. See the output contract.
Exact decimal significands/exponents now survive all AST stages. The prelude has
`showsPrec`, and `Show` instances must explicitly implement `show` or `showsPrec`.

Remaining work:

- Out-of-range `Int` literals follow wrapping `fromInteger` semantics. A checker
  warning is deferred; v0 does not diagnose this conversion yet.
- Local user modules now load, check and link end to end, including imported
  schemes/instances, aliases, kinds, constructor visibility and operator fixities.
  The workbench reuses unchanged checked imports across edits. Cyclic imports,
  re-exports, package/network resolution and persistent user-module caches remain deferred. See [the module guide](compiler/knot-workbench/modules.md).
- Exhaustiveness diagnostics report case coverage/redundancy, without missing
  pattern witnesses. Equivalent diagnostics for refutable lambda/function/do
  patterns are not complete. Parser error recovery is not implemented.

## Twine executable v0 status

The runtime is implemented in `knot-core`, `twine`, `knot-workbench` and
`knot-cli`, with the initial conformance and inspection gates passing.
See the [runtime guide](compiler/twine/README.md) for commands and API limits.

Available now:

- Core lowering and printing, dictionary arguments, instance contexts, default
  methods, superclass selection and method-specific constraints.
- Lazy closures, partial application, recursive bindings, constructors, lists,
  tuples, records, nested patterns and shared lazy pattern-let projections.
  `do` and do-let statements lower to dictionary applications and recursive lets.
- A session-owned heap with explicit continuations, memoized values **and errors**,
  black-hole detection, source-origin error traces and activation identities.
  Work, heap-entry and primitive-size limits plus cancellation abandon a session
  explicitly; interruption is not memoized as a language error.
- Wrapping `Int`, arbitrary-precision `Integer` and binary64 `Float` arithmetic
  and rendering, exact decimal literal execution through `fromRational`, correctly
  rounded ratio conversions, basic string operations and explicit dictionaries.
  Float rendering is pinned to GHC 9.6.2 goldens. Nonfinite Float-to-Integer/Ratio
  conversions are runtime errors. Exact decimal expansion is demand-driven and
  bounded by session limits (default maximum absolute exponent: 4096).
- Derived `Eq`/`Ord`/`Show` for ADTs, including recursive fields; pointwise
  `Semigroup`/`Monoid` for single-constructor types; structural `Eq`/`Ord`/`Show`
  for tuples, closed records and unit. Generated methods use checker-exported
  field evidence and preserve custom instance behavior, short-circuit comparisons
  and precedence-aware rendering, including Float and String fields.
- All declared text primitives, Haskell-style String rendering (including
  non-ASCII escapes), strict decimal text parsing and Unicode category predicates.
  Text list producers allocate lazily; concat uses machine continuations; expanding
  text operations check resource limits. Map/String public behavior and selected
  existing programs have execution tests.
- Pure IO descriptions and a separate host runner for `putStrLn`, `print`,
  `getLine`, pure and bind. Running a shared action twice repeats its effects.
- CLI `check`, `run`, `eval`, `core` and `inspect` commands load the entry file,
  its local imports and the bundled libraries. Run validates `tip :: IO Unit` and
  eval checks a `show` query before erasure.
- Rust embedding: registered nominal opaque types and pure monomorphic host
  functions with scalar/native arguments and results. The same declarations feed
  canonicalization, checking and runtime linking. Native snapshots share `Arc`
  ownership, support explicit Knot instances, and use cooperative work/storage
  budgets. See [the Mesh example and contract](compiler/twine/host-types.md).

The executable-v0 conformance and initial inspection gate now includes:

- Execution probes for all 102 supported whole-program fixtures (412 binding/query
  results, including 60 explicit function calls), broad Prelude behavior tests,
  and a full SVG drawing under default limits. Coverage is a regression gate,
  not a proof for all possible inputs.
- Checked inspector expressions prepared in a selected lexical scope, selectable
  runtime activations, non-forcing states and paginated child handles. WHNF forcing
  has a separate budget and rolls back interruption without abandoning the main
  session. It never executes IO. See the API.
- Source excerpts/demand traces, configurable CLI limits and statistics, and a
  disposable persistent cache of bundled library core. Numeric scratch storage
  is charged linearly; defaults are 50 million work units and a 16 MiB payload cap.

The inspector's query API does not yet attach code to an already-running revision:
queries are checked before evaluation. Retry after truncation restarts from the
restored thunk state, not a saved continuation. Local module loading is implemented;
revision identities and opt-in demand tracking are implemented in the
revision API. Bounded pure-graph reuse is
implemented; broader incremental scheduling follows the
archived incremental plan.

Unknown core primitives fail explicitly with source origins. There are no
remaining deferred names in the current library primitive inventory; future
library additions must provide runtime implementations or be explicitly marked
as deferred until implemented.

## Deferred beyond the initial interpreter

Cyclic module imports, re-exports, persistent user-module compilation caches,
package loading, WebAssembly/Tangle integration, more precise incremental scheduling
and automatic persistent source-query heaps, general annotation checking and the
reverse-generation gaps above, richer host bridges
(parameterized types, polymorphic functions, containers/ADTs and Knot callbacks),
custom host IO actions, geometry/resource loading and decision-tree pattern
compilation. Laziness here means call-by-need evaluation; it does not by itself
implement lazy file/asset loading.

The default evaluator is Twine. The drawing app also has an opt-in
compiled Bézier demo
with guarded native execution and interpreted fallback.

## Regression coverage

Formerly accepted custom structural-instance examples now live under
`corpus/semantic/invalid/`, including `invalid/v0-programs/`. They are asserted
rejections, not skipped tests. Positive nominal-wrapper programs demonstrate the
supported alternative. The workspace tests also cover rejected specialized heads,
superclass context entailment, constructor-interface contexts, evidence trees,
and preservation of `do` through checking.
