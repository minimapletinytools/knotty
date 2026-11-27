# Unravel design follow-ups

Updated 2026-11-26 · Open work only.

The design itself is in [unravel-design.md](unravel-design.md). What works
today, and what does not, is shown one case at a time by the
[unravelling stories](knotty-drawings/examples/unravel/); stories marked
`needs-work` are referred to below by file name.

The migration tracked here until 2026-11-16 is finished: constructor and
product rules run through the ordinary rule framework, materialization belongs
to `Constriction` instances, generated callbacks return ordinary `Maybe`
requests for one or several arguments, and `BodyRequest`, `@materialize` and
the decomposer registrations are removed. Those sections are deleted from this
file.

`Feed a` and `Unraveller a b sIn sOut` below are shorthand for the records in
unravel-design.md. The Prelude writes those records out in full; it has no such
aliases yet.

## 0. Next: splice, then lash

### Splice (implemented)

The design is in unravel-design.md §2 and §3; `tests/splice.rs` in the engine
and the story `07-04-a-rule-that-splices` show it.

Still open on splice:

- **Generated callback code never answers `Splice`.** A rule inside a lambda's
  body that says `Splice` is a failure there, because the body is shared by
  every element. The calling rule only sees a `Splice` from a callback an
  author wrote by hand.
- **A bubbled splice is all or nothing.** Edits already routed out through the
  call's arguments are dropped and the whole call is printed. Keeping the parts
  that could be routed needs the wrapping work in §7.
- **A partial request cannot be spliced.** `Splice` prints the materialized
  request, so "second element is 7, rest anything" has nothing to print. If
  that matters, `Splice` would carry a value.
- **Your own functions are not bubbled.** Only read-only source bubbles. A
  literal inside your own function is edited in place, for every caller.

### Lash (designed, to implement next)

Splice throws an expression away and prints a value. Lashing keeps the
expression and adds a call around it: `e` becomes `e |> set_stroke blue`. It is
how a property with no setter in the source gets one (`02-02-no-setter`), and
how a created value is written with the library's own functions and not as raw
data (`04-09-tidy-source-after-growth`).

**How.** Insert the call with `default` arguments, then unravel the request
through it in the ordinary way. `🧶set_stroke` routes the stroke to its
argument, whose literal is then spliced; the rest goes to `e`, which already
fits. `growMap` does the same with a callback. Creating `circle 160 80 25` is
the same step with nothing wrapped: insert `circle default default default`
and unravel through `🧶circle`.

**Which function.** A library marks the functions that may be lashed on with
`@lasher`. For each, in the order marked, the engine:

1. calls its rule with default arguments, the current value of `e` as the last
   argument, and the request;
2. calls `fits` on the request the rule returns for that last argument and the
   current value of `e`.

The first function for which that holds absorbs what was missing. If it leaves
part of the request unmet, repeat with what remains. An application may also
name the function outright for one edit.

Conventions and limits: the wrapped expression is the last argument, as `|>`
feeds it. The rule is run on a default argument, the same assumption `growMap`
makes. A rule that asks nothing of the wrapped argument passes for any request,
so marking and ordering lashers is the library author's care. The test only
picks the function; the final check still decides whether the edit is right.

**Where.** The very end by default: the outermost expression that produces the
value. An author moves it with `@cleat` on an expression:

```knot
circle 120 80 30 |> @cleat set_fill tomato |> rotate 30
```

Later, perhaps: named cleats such as `@cleat("style")`, so a whip can aim at
one of several.

**When: the caller's strategy.** An edit from the application carries a
request and a strategy. A whip's control is data the application reads, so it
can carry the strategy, as in `@whip(LashFirst (ColourPicker "Outline"))`; the
canvas and the properties pane choose per property or gesture.

| Strategy | Meaning |
| --- | --- |
| Unravel first (default) | Go all the way back. On a snag, lash at the nearest cleat passed on the way, or at the very end. |
| Lash first | At the first cleat reached, try the lashers. If none absorbs the request, carry on unravelling. |
| Splice here | Replace the expression the edit lands on with the printed value. |

Lash first can duplicate a setter: with `set_fill red` inside the cleat, a blue
fill adds a second `set_fill` and leaves the red one. Setters outside the cleat
are reached first and edited as usual.

Build splice-at-an-expression in the engine so that both a rule's outcome and
a caller's strategy can invoke it.

### The same fork, still decided by fixed policy

- A reference to a top-level value always edits the definition, never the one
  use. Detaching one shape from a shared size is the "splice here" strategy
  above.
- `if` always follows the branch that ran (§6, branch choice).
- Replacement throws away the argument expressions (§7, wrapping).

### Requests that may be missed

The final check calls `fits` on the request. It only behaves as exact equality
because applications send exact requests such as `Just 43`. A request that is a
preference needs no engine change:

- a looser constriction type, whose `fits` accepts a range or anything and
  whose `materialize` gives the preferred value;
- rules that accept it. The default rules take `Maybe a`; a relaxed rule for
  `fromIntegral`, say, would turn a request for 43 into 40 where the source
  can only make multiples of ten (`01-10-whole-numbers-only`). Strict stays the
  default; the relaxed rule is named with a qualifier, `🧶fromIntegralRelaxed`;
- the application sending it and drawing both what was asked and what was
  given. Only the scale whip draws the second box today
  (`03-06-resize-changes-the-frame`, `03-08-scale-about-the-centre`). Every
  gizmo should be able to work this way, switched on by the application.

## 1. Constructor attachment syntax

Rules can be attached to data constructors from the host
(`SourceEditor.with_constructor_rules`), and the engine supplies defaults for
records, tuples and cons. There is no source syntax for an author to attach a
rule to a constructor.

```knot
-- Schematic: request types are chosen by the rule author.
🧶pair :: (Feed a, Feed b) -> sOut -> Maybe (sA, sB)
```

To decide: where the annotation goes in a `type` declaration, and how it reads
when a type has several constructors. A constructor's rule edits its arguments; changing
to a different constructor is replacement by the engine (§0).

## 2. Sensitivity

`Sensitivity a` is a stub type in the Prelude and the checker. Nothing is
generated from it.

- Generate recursive `Sensitivity` request types and validators for records,
  tuples of any length and ADTs; use `ListConstriction` for lists. Specify the
  generated constructor names, recursive derivation, opaque leaves and instance
  coherence. Derivation fails for function or opaque fields.
- Add `ToSensitivity` / `FromSensitivity` dispatch, tried only after direct
  rule matching fails. A failed conversion is final: no second conversion, no
  other rule.
- Enforce the converted-request-implies-original law on actual candidates by
  validating both requests.
- The name is not settled. unravel-design.md has a TODO to rename it; "tension"
  and "taut" are held in reserve for this in the style guide.

## 3. Callbacks

### Supplied arguments and captures stay fixed

In `map (f x) ys`, the callback's rule can return requests for elements of
`ys`, but not for the already supplied `x`. An edit that needs `x` to change
fails. The same holds for a value a lambda captures from outside: it is pinned.

This is the main thing the Bézier demo runs into
(`06-06-the-bezier-shapes`; `06-02-captures-stay-pinned` shows it alone). The
current way around it is a whip on the shared value (§5), or a wrapper function
with its own rule (`06-04-wrapper-with-a-rule`).

To design: unravels on function types themselves, so that a request on `f x`
can reach `x`, possibly through `f`'s own rule. Open questions:

- How shared requests are reconciled when several elements ask for different
  values of the one captured `x`.
- What provenance a generated lambda needs to carry a route back to an
  enclosing editable input. Until that exists, such captures reject; they are
  not silently treated as one more editable argument.
- (V2, to consider) Editing the callback's own code is the same idea: a
  request on the function value, unravelled into the lambda's source. In
  `map (\x -> circle x 80 18) xs`, moving one circle in `y` can only be done by
  changing the `80`, which is a request on the callback, not on any element
  (`04-01-one-element`). It is not a splice: nothing is replaced per element,
  and the change applies to every element, so the other elements' requests
  must still fit. Until then literals inside a callback stay fixed.
- Generated bodies for a partial application need a concrete source body
  today ("generated callback partial application needs a concrete source
  body").

### Function-valued inputs and results

A callback contract describes one call with a fixed number of ordinary
arguments and a non-function result. Function-valued inputs and results are
rejected.

### Recursively higher-order callbacks (specified, not implemented)

A companion can itself describe a function that accepts callbacks. Its
`forward` field keeps the original function type; its `unravel` field receives
companions for its callback arguments, then the grouped ordinary feeds, then
the output request. It returns requests for ordinary arguments only. Apply this
rule recursively.

For a callback of type `(a -> b) -> c -> d`:

```knot
type alias HigherUnraveler a b c d sA sB sC sD =
  { forward : (a -> b) -> c -> d
  , unravel : Unraveller a b sA sB -> Feed c -> sD -> Maybe sC
  }
```

The nested unary companion needs `Constriction sA a` and `Constriction sB b`;
the enclosing companion needs `Constriction sC c` and `Constriction sD d`.
For example, a rule can forward these capabilities without new runtime syntax:

```knot
useHigher :: ((a -> b) -> c -> d) -> (a -> b) -> c -> d
useHigher higher callback x = higher callback x

🧶useHigher ::
  HigherUnraveler a b c d sA sB sC sD ->
  Unraveller a b sA sB -> Feed c -> sD -> Maybe sC
🧶useHigher higher callback input request =
  higher.unravel callback input request
```

- A callback's own callback may have several arguments or itself take
  callbacks; substitute its companion shape recursively. Different levels may
  choose different request types.
- Each call site must supply compatible companion contracts or an explicit
  supported adapter. No searching for contracts, and no trying another rule
  after the selected one fails.
- This is a finite tree of checked contracts. It does not need a recursive type
  alias, rank-n polymorphism, dependent types, function equality or
  function-valued requests. Packages are monomorphic at their use sites to
  begin with; a callback that must work at several instantiations is out of
  scope.
- Callback code stays fixed at every level. A nested rule may request changes
  to ordinary inputs, not to callback code or unrelated captures.
- Dynamic callbacks must carry a companion for the closure actually selected.
  None is rebuilt from a function name or a forward type alone.
- A contract with no ordinary inputs has nothing upstream to edit. Its unravel
  signature omits the feed group and returns `Maybe Unit`; success only accepts
  an invocation that is already valid. Current preparation requires an ordinary
  input.
- Checking and generation share explicit depth, node and work limits.
  Recursive source definitions must not cause unbounded contract expansion;
  unsupported recursion rejects.

Tests to add with it: zero-value-input preparation, nested closure provenance,
contract compatibility and bounded generation.

## 4. Rules the standard library still lacks

The stories show these as the most visible gaps. None needs new engine work
unless noted.

| Function | Story | Note |
| --- | --- | --- |
| `zipWith` | `04-13-through-zip-with` | Binary callbacks are supported; the rule is not written. Which side changes is the rule's policy, informed by pins. It keeps both list shapes and leaves unpaired suffixes unconstrained. |
| `foldl` and other accumulators | | A request on the accumulator routes to the preceding call, and in the end to the initial value; a request on an element routes to that element. The rule must keep obligations across repeated calls. |
| `filter` | `04-12-through-filter` | The rule must refuse an edit that pushes an element out of the filter. |
| `reverse`, `take`, `drop` | `04-14-through-reverse` | |
| `Map` | `05-04-map-value`, `05-05-map-insert`, `05-06-map-delete`, `05-07-map-retarget` | No rules at all. Insert and delete share the list gaps in §6. Nothing chooses between editing a value and editing the key that reads it. |
| `hsl` | `01-07-colour-through-hsl` | |
| Ranges such as `[0..n]` | `06-06-the-bezier-shapes` | A range's elements cannot be edited one by one. |

## 5. Whips

`@whip` is implemented in the drawing application, not in the engine. The
engine parses the annotation and hands it over; the application evaluates the
control, places it and turns a gesture into a request on the bound value.

Working today: `IntSlider` and `ScaleHandle` controls; `OnNode` and `OnNodes`
anchors; a live preview for every whip through
the same preview session as other edits.

Decided but not exercised: several whips on one expression are allowed.

Open:

- **An engine-level contract.** Expose evaluated anchor references and an
  evaluation identity, so an application can resolve repeated or absent output
  occurrences and reject or refresh stale handles. Define evaluation
  dependencies before execution; do not introduce output cycles. With two
  controls written, it is time to decide what is general and what is the
  drawing library's own.
- **Non-proportional scale handles.** `ScaleHandle` asks for a new value by
  proportion, which is only right when size is proportional to the bound
  value. Anything else needs a numeric solve.
- **More controls.** A colour control (`09-06-a-colour-whip`) and a point
  handle bound to two numbers (`09-05-a-position-handle`).
- **Whips on local values.** `@whip` attaches to top-level bindings only
  (`09-07-a-whip-on-a-local`).
- **Gestures that change their own frame.** A resize that also rescales the
  view can never follow the pointer, so the canvas refuses it
  (`03-06-resize-changes-the-frame`). Related: no canvas gesture asks only for
  a size and lets the position land where the program puts it
  (`03-08-scale-about-the-centre`); the scale handle does this for a whip only.

## 6. Lists: growth, insertion, deletion, order

- **Growth through `map`.** `growMap` grows and uses `Default`; plain `map`
  keeps the list's shape. Still to decide: which of the two should be the
  default spelling, and a naming scheme for "same function, different unravel".
  Making plain `map` grow needs the engine to look up `Default` optionally, so
  that element types without one still unravel.
- **Insertion in the middle** (`04-05-insert-in-the-middle`). A list grows only
  at its end. Insertion needs a request that says where, and a way to choose it
  in the editor.
- **Deletion** (`04-06-delete-an-element`). Rules keep or grow a list's length;
  a request cannot ask for a shorter list.
- **Reordering** (`04-07-reorder-elements`). Elements are matched by position.
  Reordering needs matching by identity, so a moved element keeps its source
  expression.
- **Branch choice** (`08-04-create-the-other-branch`). The default value fixes
  which branch of a callback runs, so creation only reaches outputs on that
  branch. Lifting this needs the engine to try other branches, not a different
  default.
- **`@stock(value)`** (V2, to consider). An annotation on a function that
  overrides the default for its input, for when one type needs different
  starting values in different places. Deferred because `materialize`
  instances are ordinary code: they can call `default` but cannot see an
  annotation, so the two would disagree. Open questions if revisited: a wrapper
  such as `\d -> f d` loses the inner function's stock; what the expression may
  refer to on a local lambda; several arguments take a tuple.

## 7. Source edits

- **Replacement boundaries.** Settle the primitive-type TODO in
  unravel-design.md. The implementation already goes further than the design
  text: it reconstructs supported nominal data at editable read-only
  references.
- **Wrapping existing source.** A policy that stops traversal and wraps what is
  there, such as `original <> [newItem]`. Candidate values alone cannot keep an
  original expression or pick a particular spelling of a call.
- **Inserting a call that is not there** (`02-02-no-setter`). The editor
  changes arguments that exist; it cannot add a `set_fill` to a shape that has
  none.
- **Printing new values as code** (`04-09-tidy-source-after-growth`). A created
  value is printed from its data on one line, not as the code a person would
  write.

## 8. Types Knot cannot express yet

The engine generates the default rules for records and tuples because Knot
cannot write a transformation over an arbitrary record's fields, or a tuple of
any length, that produces the matching request shape. That needs type-level
mapping over rows. Prefer Knot-authored definitions if the language gains it.
