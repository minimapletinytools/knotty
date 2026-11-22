# Unravel design follow-ups

Updated 2026-11-16 · Agreed direction; migration in progress.

## Unify constructor and product unraveling

Use ordinary unravel rules on data constructors. The separate constructor/product
decomposer protocol is removed.
Constructor identity, source field locations and reconstruction remain engine work;
request policy, dispatch and validation use the same framework as function unravels.

For example, a constructor `Pair :: a -> b -> Pair a b` has a rule shaped like:

```knot
-- Schematic: request types are chosen by the rule author.
pairBack :: (Feed a, Feed b) -> sOut -> Maybe (sA, sB)
```

Its input requests require the corresponding `Constriction` relations. Constructor
annotation syntax still needs defining. Changing constructor variants remains a
materialization operation; editing arguments preserves the existing constructor.

Implemented: `SourceEditor.with_constructor_rules` attaches ordered ordinary rules
to constructors with fields. Source traversal and generated callbacks share function
rule checking and selection; source calls also share proposal execution and pins.
Standard cons rules and tuple/record defaults now use this path too, including
record updates and generated callbacks. Empty products have no editable arguments.
Legacy constructor/family and product registrations are removed. Custom products
use `ProductRule` with per-field feeds (alphabetical for records); the drawing
app uses `ConstructorRule`. Exact nominal defaults share rule checking/execution,
with variant changes handed to the engine replacement boundary.

## Keep FieldRequest, make it general

```knot
type FieldRequest s = NoRequest | RequestField s
```

This remains useful when a constraint has no unconstrained case of its own:
`NoRequest` skips an input without forcing it; `RequestField s` routes `s` normally.
It must work through the unified unravel framework, not a separate decomposer-only
result convention. Ordinary requests remain valid without this wrapper.

Implemented: `Constriction s a => Constriction (FieldRequest s) a`.
`NoRequest` accepts any value; `RequestField s` checks `fits s`. Materialization
returns `Nothing` for `NoRequest`, otherwise delegates to the inner request.
Ordinary rules, product fields, output requests and generated callbacks unwrap it.
Skipping still requires the inner relation and does not waive pins, branch guards
or final output validation. Custom instances overriding this wrapper now conflict.

## Records and tuples

Provide engine-generated default unravel rules for record construction/fields and
tuples, using the same checked contract, dispatch and validation as author rules.
Their requests may contain `FieldRequest` per field; this is not a second framework.

Concrete record request types can already be written in Knot. What Knot cannot
currently express is a generic transformation over an arbitrary record's fields
(or an arbitrary-length tuple) that produces the corresponding request shape.
That needs structural type-level mapping/row machinery, not just an ordinary
higher-kinded parameter. Generate those shapes in the engine for now; prefer
Knot-authored definitions if the language later gains the required expressiveness.

## Simplify generated callback requests

Remove `BodyRequest` from the default callback protocol. Generated callback
unravelers use `Eq a` and ordinary `Maybe a` input constraints:

```knot
-- Default generated callback contract:
callbackBack :: Eq a => Feed a -> sOut -> Maybe (Maybe a)

-- Generic map rule remains parameterized by the callback's constraint types:
mapBack :: Constriction sIn a =>
  Unraveler a b sIn sOut -> Feed (List a) ->
  ListConstraint sOut -> Maybe (ListConstraint sIn)
```

For the generated default, `sIn = Maybe a`: outer `Nothing` means failure,
`Just Nothing` means no input constraint, and `Just (Just candidate)` requests
an equal input. Accept ordinary `Eq` semantics for this default; do not add a
runtime-injected predicate to its request type.

Authors may provide custom `Constriction` types and callback unravelers when
exact requests are insufficient. Generic `map` forwards the supplied callback
contract rather than requiring `BodyRequest` or any other particular input
constraint representation.

Remove the `BodyRequest` type, its adapter and dedicated selectors after migrating
generated callbacks, Prelude map/concatMap/growth policies, materialization
examples and tests to ordinary exact requests or explicit author-defined policies.
Retain the source editor's normal output validation, pins and transaction checks;
this decision does not introduce a replacement public guard-carrying request type.

Implemented: unary and multiargument generation use ordinary `Maybe` requests.
Internal guards check proposals before returning; final transactions validate
retained requests and pins. Prelude map/concatMap are generic over callback
constraints. ConcatMap checks chunk lengths using an instance-materialized input
candidate; generated exact requests supply one. `BodyRequest` and its adapter
and selectors are removed from code, examples and current implementation docs.

## Materialization and sensitivity migration

- Add optional `materialize :: s -> Maybe t` to `Constriction`, defaulting to
  `Nothing`. Validate returned candidates with `fits`; retain normal transaction checks.
- Replace selector/`@materialize(template)` registration with instance-owned
  candidate construction. Drafts remain ordinary Knot helpers, with no special
  engine protocol. Keep source locations and reconstruction
  in the engine. Decide standard list/product materialization policies explicitly.
- Generate recursive `Sensitivity` request types and validators for records,
  arbitrary tuples and ADTs; use `ListConstraint` for lists. Specify generated
  constructor names, recursive derivation, opaque leaves and instance coherence.
- Add fallible sensitivity adapters only after direct rule matching fails. Enforce
  the converted-request-implies-original law on actual candidates by validating
  both requests; no arbitrary conversion search or missing-validator fallback.
- Implement multiargument callback companions using grouped per-argument feeds,
  requests and validation evidence, as proposed in the callback contracts below.
  Keep unary `map` generic over author constraint types; generated defaults use
  `Maybe a` per ordinary callback input.
- Keep recursively higher-order companion execution/generation deferred. Its
  proposed recursive contract and fixed-capture semantics are recorded below; nested
  calls inside an otherwise unary callback are a separate existing capability.
- Separately design policies that stop traversal and wrap existing source, such as
  `original <> [newItem]`. Candidate values alone cannot preserve an original
  expression or select a particular factory-call spelling.

## Proposed higher-order callback contracts


**Proposal for manual integration into the design, not implementation status.** Multiargument callbacks are required
for the next companion extension. Recursively higher-order callback contracts are
specified below but their implementation is deferred. The bridge now accepts
unary and multiargument callbacks. Nested higher-order calls inside a callback
body are distinct from callbacks whose own parameters are functions.

### Unary callback companions

```knot
type alias Unraveler a b sIn sOut =
  { forward : a -> b
  , unravel : Feed a -> sOut -> Maybe sIn
  }
```

A higher-order rule receives an `Unraveler` for each callback instead of a `Feed`.
These packages come first, in callback argument order, followed by the grouped
feeds for ordinary arguments and the output request. One ordinary argument has
one feed; multiple ordinary arguments have a tuple of feeds. Returned requests
cover only those ordinary arguments, in their original order.

For now, callback code and already supplied arguments stay fixed (see the accepted
partial-application limitation below).
The attached rule's signature specifies
the callback request types; `a -> b` alone cannot determine them. A companion
retains the actual forward closure and the corresponding unravel closure, with
their selected dictionaries and captures. Function values need no `Eq` or
`Constriction` instance.

The default Prelude `map` rule stays generic:

```knot
mapBack :: Constriction sIn a =>
  Unraveler a b sIn sOut -> Feed (List a) ->
  ListConstraint sOut -> Maybe (ListConstraint sIn)
```

Generated unary callback unravelers use `sIn = Maybe a` and require `Eq a`.
Author-written companions may choose other constraints. There is no `BodyRequest`
in this intended design. Ordinary `map` preserves shape; growth requires an
explicit policy.

### Multiargument callbacks

The same grouped-feed convention applies within a companion:

```knot
type alias Unraveler2 a b c sA sB sOut =
  { forward : a -> b -> c
  , unravel : (Feed a, Feed b) -> sOut -> Maybe (sA, sB)
  }
```

The checker requires `Constriction sA a`, `Constriction sB b` and
`Constriction sOut c`. Each input has its own pin. Outer `Nothing` fails the
whole invocation; `Just (requestA, requestB)` proposes both input requests
jointly. A proposal is not a candidate or a proof: the transaction must satisfy
both requests and pins, and validate the actual forward result.

For example, a binary callback can be passed to `zipWith`:

```knot
zipWithBack :: (Constriction sA a, Constriction sB b) =>
  Unraveler2 a b c sA sB sOut ->
  (Feed (List a), Feed (List b)) ->
  ListConstraint sOut -> Maybe (ListConstraint sA, ListConstraint sB)
```

The rule can invoke `callback.unravel (leftFeed, rightFeed) elementRequest` and
route the returned constraints to the corresponding list elements. Which side
changes is rule policy, informed by pins; the engine does not automatically favor
one side. Ordinary zip-with preserves the input list shapes, produces the shorter
length, and leaves unpaired suffixes unconstrained. Growth needs an explicit policy.

For an accumulator combinator such as `foldl`, an accumulator request routes to
the preceding callback invocation (eventually the initial accumulator), while an
element request routes to that element's source. Intermediate accumulators are
computed values, not independently editable leaves. The combinator's author
supplies this reverse policy and retains obligations across repeated calls.

For three or more ordinary inputs, extend the feed and request tuples in argument
order, without an arity cap. `Unraveler2` is a convenience alias, not a distinct
runtime protocol; the engine checks record shapes rather than alias names.

Implemented for author-written `CallbackFactory` companions: grouped feeds and
requests, per-input validators and pins, lazy skipped inputs, and invocation arity
in checked metadata. These also work inside generated unary callbacks.
Generated multiargument companions now solve inputs jointly, refresh affected
values between requests, and preserve independent pins and lazy skipped inputs.
Unary and multiargument generation both return ordinary `Maybe` requests.
Function-valued inputs/results remain unsupported.

Generated multiargument companions return a tuple of optional exact requests:

```knot
-- Generated default for a -> b -> c:
callbackBack :: (Eq a, Eq b) =>
  (Feed a, Feed b) -> sOut -> Maybe (Maybe a, Maybe b)
```

This is a schematic generated signature: `sOut` is fixed by the selected
contract, not a promise that one generated body can invert every request type.

`Just (Nothing, Just y)` constrains only the second input. The generator must
solve the body over all input roots together, refresh values after proposals,
respect shared source and pins, and validate the full callback result. Running
two independent unary inverses is insufficient. Any internal obligations needed
by generation remain transaction validation responsibilities, not fields added
to this public request type. Candidate selection and budgets remain explicit;
this extension does not make arbitrary predicates invertible.

Feed/request tuples retain ordinary lazy semantics. Inspecting one component
does not force every input; an unconstrained component need not inspect its feed.
Independent pins and final validation obligations still apply.

### Arity, currying and partial application

The companion contract describes one invocation with a specified number of
arguments and a non-function result. `a -> b -> c` is a curried binary invocation;
`(a, b) -> c` is a unary invocation with one tuple-valued feed. These contracts
are not interchangeable. Invocation arity/argument roles are retained in checked
metadata and matched against the companion's unravel signature; they must not
be guessed by splitting source lambdas or accepting the first matching arrow.
Forward types and requests must determine all contract types before selection;
arity handling does not introduce new inference from runtime values.

**Accepted limitation:** in `map (f x) ys`, the companion for `f x` can return
requests for elements of `ys`, but not for the already supplied `x`. It stays fixed;
an edit requiring a change to `x` fails. Supporting multiple callback arguments
does not remove this limitation.

**Future spec:** investigate unravels on function types themselves to handle partial
applications. How a request on `f x` reaches `x` (possibly through `f`'s unravel),
and how shared requests are reconciled, remains to be designed. The earlier
source-linked proposal channel is not a settled requirement.

### Recursively higher-order companions (specified, implementation deferred)

A companion can itself describe a function accepting callbacks. Its `forward`
field keeps the original function type; its `unravel` field receives companions
for its callback arguments, then grouped ordinary feeds, then the output request.
It returns requests for ordinary arguments only. Apply this rule recursively.

For a callback of type `(a -> b) -> c -> d`:

```knot
type alias HigherUnraveler a b c d sA sB sC sD =
  { forward : (a -> b) -> c -> d
  , unravel : Unraveler a b sA sB -> Feed c -> sD -> Maybe sC
  }
```

The nested unary companion needs `Constriction sA a` and `Constriction sB b`;
the enclosing companion needs `Constriction sC c` and `Constriction sD d`.
For example, a rule can forward these capabilities without new runtime syntax:

```knot
useHigher :: ((a -> b) -> c -> d) -> (a -> b) -> c -> d
useHigher higher callback x = higher callback x

useHigherBack ::
  HigherUnraveler a b c d sA sB sC sD ->
  Unraveler a b sA sB -> Feed c -> sD -> Maybe sC
useHigherBack higher callback input request =
  higher.unravel callback input request
```

A callback's own callback may have several arguments or itself take callbacks;
substitute its corresponding companion shape recursively. Different levels may
choose different request types. Each call site must supply compatible companion
contracts or an explicit supported adapter; do not search for contracts, silently
change them, or try another rule after selected-rule failure.

This is a finite tree of checked capability contracts, not a recursive Knot type
alias or a requirement for rank-n polymorphism, dependent types, function equality
or function-valued requests. Initially, packages are monomorphic at their use sites;
ordinary outer type variables may specialize them. Polymorphic callbacks requiring
one package to work at multiple instantiations are outside this extension.

Fixed-callback semantics apply at every level. A nested rule may request changes
to ordinary inputs, including already supplied arguments with source routes,
but not to callback code or unrelated captures.
Generated lambdas capturing an enclosing editable input need explicit provenance
and a route back to that input; until that route exists they reject, rather than
silently treating the capture as another editable argument. Dynamic callbacks
must carry compatible companions for the actual selected closure; no package is
reconstructed from a function name or a forward type alone.

Contracts with no ordinary inputs have no upstream editable value. For completeness,
their unravel signature omits the feed group and returns `Maybe Unit`; success can
only accept an already valid fixed invocation, not manufacture an editable function.
Executing an unnecessary rule is still avoided when a request is already satisfied.
This degenerate case is also deferred; current preparation requires an ordinary input.

Recursive checking and generation must share explicit depth/node/work limits.
Recursive source definitions must not trigger unbounded static contract expansion;
unsupported recursion rejects. Companion execution remains ordinary lazy Knot calls,
with the normal runtime limits and final atomic source-edit validation.

## Multiargument callback implementation

- Replace the unary assumption in `check_companion` with a checked invocation
  contract: argument positions/roles, ordinary input types, per-input constraints
  and validator evidence, and a non-function output/request pair. Preserve the
  distinction between a unary tuple input and several curried arguments.
- Thread that contract through explicit companion preparation, inferred contracts,
  source factories and generated bodies. Supplied arguments remain fixed for now;
  extending partial applications is deferred to the function-type unravel design.
- Generate bodies over multiple input roots jointly, returning tuples of optional
  exact requests with per-input Eq evidence. Preserve shared-input obligations,
  pins, branch validation and final forward-output validation without reintroducing
  `BodyRequest` as a public requirement.
- Test binary/ternary callbacks, unary tuples, mixed callback/value positions,
  repeated argument types, static partial applications, custom request types,
  ignored/erroring inputs, per-input pins, shared inputs and conflicting requests.
  Exercise a binary combinator such as zipWith and an accumulator combinator such
  as foldl; repeated invocations must retain earlier obligations. Partial-application
  tests should preserve supplied arguments under the accepted limitation.
- Add rejection tests for unsupported function results, unresolved contract types,
  mismatched companions and nested higher-order contracts until implemented.
  Future recursive contracts additionally need zero-value-input preparation,
  nested closure provenance, contract compatibility and bounded generation.

## Implementation checklist

- [x] Direct matches follow attachment order.
- [x] Instance-owned materialization, including structural products and lists.
- [x] General `FieldRequest` validation and lazy no-demand routing.
- [x] Shared constructor/product/cons rule checking and execution, including
  per-field product rules and exact nominal defaults.
- [x] Unary and multiargument generated companions return ordinary `Maybe` requests.
- [x] Generic map/concatMap contracts; updated growth, examples and drawing integration.
- [x] Preserve source routes, pins, budgets, atomic validation and undo.
- [x] Remove legacy selector/template and decomposer registration APIs.
- [ ] Source constructor attachment syntax.
- [ ] Sensitivity generation/conversions.
- Deferred: function-type unravels, including editable supplied callback arguments.
- [ ] Whips and the remaining replacement-boundary decision.

This tracks implementation follow-ups to [unravel-design.md](unravel-design.md).

## Migration checkpoint — 2026-11-16

Direct matches follow attachment order. Instance-owned materialization replaces
selector and source-template registration. Exact, alternative, list and structural
product requests supply candidates; failed construction leaves source unchanged.
Current-value policies belong in unravel rules. Breaking API changes are welcome;
there is no compatibility requirement.

Settled decisions:

- Supplied callback arguments stay fixed for now. Function-type unravels are a
  future design direction for lifting this accepted limitation.
- Conversion failure is final; do not retry another conversion or rule.
- Apps default to `Sensitivity T` (`Maybe T` for primitives), and `ListConstraint`
  for lists. Rules may choose other constriction types. Recursive ADT sensitivity
  has constructor-shaped requests; derivation fails for function or opaque fields.
  The brief design clarification is merged in PR #102; generation remains to implement.
- Multiple whips on an expression are allowed. Apps define anchors, UI, placement
  and behavior across program revisions. Detailed whip execution is deferred.

Remaining work/decisions:

- Implement sensitivity generation and fallible conversion dispatch.
- Constructor attachment syntax: generated sensitivity request constructors do not
  specify how an author attaches an unravel rule to a value constructor.
- Whip API: expose evaluated anchor references and evaluation identity so apps can
  resolve repeated/absent output occurrences and reject or refresh stale handles.
  Define evaluation dependencies before execution; do not introduce output cycles.
- Replacement boundaries: settle the primitive-type TODO; current implementation
  also reconstructs supported nominal data at editable read-only references.

Recursively higher-order execution and source wrapping remain deferred.
