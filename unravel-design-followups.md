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

### Lash (first part implemented)

The design is in unravel-design.md §3. Splice throws an expression away and
prints a value; lashing keeps the expression and adds a call around it.

Implemented (`tests/lash.rs` in the engine, stories `02-02-no-setter` and
`02-04-setter-cannot-clear`):

- `@lasher` marks a function that may be added. A bare `@name` annotation now
  parses, with arguments only when `(` follows the name directly.
- When a rule snags, the engine finds the outermost expression of that value's
  pipeline, inserts `|> lasher default ...` there, and unravels the whole
  request again through the new call. Lashers are tried in the order they are
  declared.
- A lasher that leaves the same request to snag again took nothing, and the
  next one is tried. Several missing parts take several lashers, up to four.
- The lashing and the unravel through it are reported as one edit.
- The drawing library marks fifteen setters, and its shape rules now snag when
  a request asks for style the shape function cannot provide. Before, they
  answered for their geometry and the edit failed later in the final check.

How it differs from the design text:

- **Choosing a lasher.** The design says: the first whose rule leaves the
  wrapped expression a request it already fits. The engine tries each in turn
  and keeps the first for which the whole edit succeeds. The result is the
  same where both apply; the engine's way costs a re-check of the program per
  lasher tried.
- **Only a snag triggers it.** A rule that answers `Unravel` but ignores part
  of its request fails in the final check, and nothing is lashed. Rules have to
  snag honestly for lashing to help.

Not built yet:

- `@cleat`. It parses, and the engine ignores it: a lash always goes at the
  very end.
- The strategy an application sends (unravel first, lash first, splice here).
  Only unravel first exists.
- Creating a value with a lasher (`circle default default default`), which is
  what `04-09-tidy-source-after-growth` needs.
- Lashers whose arguments have no `Default` are skipped. In the drawing
  library that leaves out `set_fill_paint`, the enum setters and the
  transforms.
- An unqualified name is used when the entry module exposes it; otherwise the
  module's name or alias is written in front.

### The same fork, still decided by fixed policy

- A reference to a top-level value always edits the definition, never the one
  use. Detaching one shape from a shared size is the "splice here" strategy
  above.
- `if` always follows the branch that ran (§6, branch choice).
- Replacement throws away the argument expressions (§7, wrapping).

### Converging requests inside one request (implemented)

unravel-design.md §3, "Branching and converging". `tests/converge.rs` in the
engine and the stories in group 10 show it.

How it is built:

- The ordinary single traversal runs first. The passes start when it fails
  with a conflict (two different demands on one source value) or in the final
  check (a part of the request that held when visited was broken by a later
  change, as when a drag moves x and asks y to stay).
- Within a pass, an exact request that already fits holds the source values
  under it until the pass ends, so a later request cannot break it.
- Each pass unravels from the output and stops at the first value it changes.
  That value, and each expression on the way whose request now fits, is
  frozen. The program is evaluated again and the next pass starts over.
- An input is pinned when every source value under it is frozen. A literal
  that has not been frozen keeps its input unpinned.
- If the passes fail, they are tried once deciding values in the other order.
- Branch guards from every pass are kept to the end.

Open:

- **A free literal inside a shared expression defeats the pin.** In
  `(tens * 10 + ones, tens)` asked for `(47, 4)`, `tens * 10` still contains
  the free `10`, so `+` is not told its left side is pinned. It asks that side
  for 45, which fails, and nothing goes back to try `ones`. A rule would have
  to be offered its inputs in turn.
- **A value neither branch asks for.** `base * 2 + base` asked for 36 needs
  `base = 12`; left first gives 13, right first gives 16.
- **Requests the passes drop.** Only frozen values and branch guards carry
  from one pass to the next. A demand the first traversal placed on a value
  the request never mentioned is not carried. `(make shared).x` asked for 10
  with `make x = { x = x * 2, keep = x }` used to be refused because `keep`
  would change; it is accepted now, since `keep` is not in the output.
- **A rule's wrong proposal can be made up for.** A rule that proposes a bad
  value for one input is not accepted as it stands, but the passes freeze that
  value and the rule, seeing it pinned, may reach the request through another
  input. The result fits the request and keeps the odd value.
- **A pinned single input still runs its rule.** A rule with one input takes
  the plain value and never sees a pin. The design says the engine need not
  call it when that input is pinned; it still does, and holds the pin
  afterwards, which gives the same result.
- **Plain or feeds is decided by how the rule is written.** For several
  inputs, a tuple of `{ value, pinned }` records means feeds and anything else
  means plain values. A rule over plain values whose inputs are themselves
  such records would be misread.
- Frozen is by source node. A value reached in two activations of one function
  is one node.

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

`reverse`, `take`, `drop`, `filter`, `zipWith` and the drawing library's `hsl`
have rules now. `filter`'s rule takes its test as a plain function, so the test
needs no reverse rule (see unravel-design.md §4). Still missing:

| Function | Story | Note |
| --- | --- | --- |
| `foldl` and other accumulators | | A request on the accumulator routes to the preceding call, and in the end to the initial value; a request on an element routes to that element. The rule must keep obligations across repeated calls. |
| `Map` | `05-04-map-value`, `05-05-map-insert`, `05-06-map-delete`, `05-07-map-retarget` | No rules at all. Insert and delete share the list gaps in §6. Nothing chooses between editing a value and editing the key that reads it. |
| `enumFromTo` and other ranges | `04-15-generated-list`, `06-06-the-bezier-shapes` | A rule could change the ends, never one element in the middle. |
| `take` and `drop` changing their count | | Asking for a longer or shorter result could change the count. Today it is a snag. |

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
- **Insertion, deletion and reordering** (`04-05-insert-in-the-middle`,
  `04-06-delete-an-element`, `04-07-reorder-elements`). See the proposal below.
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

### Proposal: a list request that says where each element came from

A `ListConstriction` describes the new list position by position. That is
enough to say what the list should be, and `fits` only needs that. It is not
enough to edit source well: asking `[a, b, c]` to become `[a, c]` reads as
"change `b` into `c`, then drop the last element", so `b`'s expression is
rewritten to look like `c` and `c`'s own expression is thrown away.

Keep `ListConstriction` as it is and add a second request type that names the
origin of each element:

```knot
type Placed s = From Int s | New s
type ListArrangement s = Arranged (List (Placed s)) | Unarranged
```

- `From i s` is the element that was at index `i`, now fitting `s`.
- `New s` is an element that did not exist.
- An old index that appears nowhere is deleted. An index may appear once.
- `fits` checks only the result: the new list has one element per entry, each
  fitting its `s`. So it is an ordinary `Constriction`.

`[a, b, c]` to `[a, c]` is `Arranged [From 0 any, From 2 any]`. Swapping two
elements is `Arranged [From 1 any, From 0 any]`. Inserting in the middle is
`Arranged [From 0 any, New s, From 1 any]`.

What each piece does with it:

| Where | Behaviour |
| --- | --- |
| A list literal | The engine rebuilds the literal: each `From i` keeps expression `i` and unravels `s` into it; each `New s` is created (with a lasher, §0); the rest are removed. |
| `map` | `From i s` runs the callback's rule on element `i`; the arrangement of the input is the same shape. `New s` needs growth, so plain `map` snags and `growMap` creates from `default`. |
| `<>` | Split by old index: entries from the left list stay left. A `New` between the two goes to the side the rule prefers, informed by pins. |
| `filter` | `From i` maps back to the input index it came from. Deleting removes the input element. `New` snags: the rule cannot know the element would pass. |
| A range or other computed list | Snags, as now. |

Open questions:

- Whether `ListArrangement` replaces `ListConstriction` or sits beside it with
  a conversion. Positional requests are still the natural thing for "the third
  element's radius is 6".
- How the application identifies elements. The drawing app keys nodes by
  position today, which is what makes a reorder look like two edits.
- `Map`: deleting or inserting a key is the same idea keyed by key instead of
  index. Decide lists first.
- Editor gestures: delete, reorder in the layers view, insert at a position.

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

## 9. Editor work the stories asked for

None of this needs unravel changes.

| What | Story |
| --- | --- |
| Type into text on the canvas | `01-08-text-and-size` |
| Edit an id in the layers view, not only in properties | `02-05-optional-id` |
| A rotate gizmo | `03-02-inside-a-rotated-group` |
| Add and remove points on a polyline or path | `03-04-polyline-points`, `04-08-add-a-point` |
| Gizmos that show both the box asked for and the box that resulted | `03-06-resize-changes-the-frame`, `03-08-scale-about-the-centre` |
| One name for a rect's corner radius in place of `rx` and `ry` | `03-03-shared-corner-radius` |
| One `set_fill` taking a paint, in place of `set_fill` and `set_fill_paint` | `02-04-setter-cannot-clear` |
| Grown lists printed as plain lists, not a `twineConstruction` `let` | `04-03-append-a-record` |
