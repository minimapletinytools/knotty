# Unravel design

Intended design · 2026-11-26. Open work is tracked in
[follow-ups](unravel-design-followups.md). Working examples are in the
[unravelling stories](knotty-drawings/examples/unravel/).

Unravel rules translate output requests into input requests. The engine turns these into source changes and validates the output.

## 1. Constrictions and materialization

```knot
interface Constriction c t where
  fits :: c -> t -> Bool
  materialize :: c -> Maybe t
  materialize _ = Nothing
```

`fits c value` checks if `value` satisfies the constriction `c`. `materialize c` optionally creates a fresh
`value`, which must pass `fits c value`. `Nothing` means no materialization available, not that the constriction is impossible. 

We can refer to the value `c` as a "request".

### Defaults

```knot
interface Default a where
  default :: a
```

`default` is the value to start from when one has to be created and there is no current value to adjust.
A reverse rule runs against a real forward evaluation, so creating an input needs a starting input first:
run the callback on `default`, reverse the request through it, then materialize the result.

- `materialize` turns a request into the value it describes. `default` supplies what no request describes.
  An instance's `materialize` may call `default` for the parts its request leaves open.
- Built-in types have defaults: zero for numbers, `False`, `""`, `[]`, `Nothing`.
- Tuples, closed records and unit take theirs from their parts, like `Eq` and `Show`.
- Any other type declares an instance. There is no derived default, so a sum type's author picks the constructor.
- A missing instance is not a type error where growth is optional. The edit that needs it is rejected.

## 2. Feeds and Unravels

A function can be annotated with one or more "unravel" function.

For a non higher order function:

for any `sOut` and `sIn` where `Constriction sIn a` and `Constriction sOut out` instances exist:

```knot
f :: a -> out
some_valid_unravel_for_f :: Feed a -> sOut -> Unravelled sIn
```

similarly

```
g :: a -> b -> out
some_valid_unravel_for_g :: (Feed a, Feed b) -> sOut -> Unravelled (sA, sB)
```

Requires `Constriction sA a`, `Constriction sB b` and `Constriction sOut out`.

The result type

```knot
type Unravelled s = Unravel s | Splice | Snag
```

- `Unravel s` requests `s` of the inputs
- `Splice` replaces the expression the rule was called on (the whole call, including its arguments). The engine materializes the rule's output request and prints that value as source. If the request does not materialize the edit fails
- `Snag` fails the edit

`Unravelled` is a monad, `Splice` and `Snag` both end a `do` block.


The type

```knot
type alias Feed a = { value : a, pinned : Maybe a }
```

describes the current input value, refreshed after speculative edits.
If `pinned = Just p`, the returned input request `s` must satisfy `fits s p`.
The engine must also preserve the actual pinned input. No condition on
`materialize s` is needed: it may choose another acceptable value or return `Nothing`.

Attach rules using `@unravel(🧶rule)` on functions or data constructors. A rule is named 🧶 plus the name of the function it unravels.

### Data constructors

A data constructor is unravelled like any other function, e.g.

```knot
-- Pair :: a -> b -> Pair a b
🧶pair :: (Feed a, Feed b) -> sOut -> Unravelled (sA, sB)
```

- the rule edits the constructor's arguments, the existing constructor is kept
- when the default constructor unravel sees a request for a different constructor it returns `Splice`. A rule you attach yourself can do the same
- the engine tracks constructor identity, source field locations and reconstruction

The engine generates default rules for records, tuples and cons. These go through the same checks as a rule you write. They are generated in the engine because Knot can not yet express a transformation over the fields of an arbitrary record or tuple.

TODO source syntax for attaching a rule to a data constructor

### Unravel selection

The unravel function to be used is chosen in the following order

1. Prefer rules accepting the output request type directly, including polymorphic specialization.
2. Otherwise use declared `ToSensitivity` / `FromSensitivity` conversions (§5).
3. Break ties within each group by annotation order. No ranking by input matches.
4. If the selected rule returns `Snag`, fail; do not try another rule.


## 3. Unravelling

Given a "request" a basic unravel is as follows:

1. Retain the request for final validation. If already satisfied, leave its source.
2. Otherwise, run the selected unravel through the source tree.
3. At a permitted replacement/creation boundary, call `materialize`, check `fits`,
   and construct source. Preserve surrounding expressions.
4. Recheck and evaluate the candidate program. Publish one atomic edit only if
   every request and retained pin/branch obligation passes. Apply/undo check revisions.

Boundaries include literals, editable uses of read-only values, any expression whose rule returns `Splice`, and structural
replacement such as a missing list suffix. Imported
library definitions are read-only. 

An unravel that reaches a literal is effectively a `Splice`: there are no inputs to request anything of, so the literal is replaced by the materialized request.

A function with no rule is unravelled through its body. A splice on read-only source can not be done in place, so it bubbles up the call until it reaches source that can be edited. Only a splice bubbles up. An `Unravel` or a `Snag` from a rule is never turned into a splice.

List insertion can preserve existing expressions through constructor-context recipes.

The final validation checks `fits` on each request and the final validation is accepted if it passes. Since fits is a looser constraint than equality, applications should be able handle the unequal case when requesting a loose constriction.

### Lashing

A splice throws the expression away. A lash keeps the expression and adds a call around it, e.g. `e` becomes `e |> set_stroke blue`.

To lash a function on, the engine inserts the call with `default` arguments and then unravels the request through it as usual. e.g. `e |> set_stroke default`, then `🧶set_stroke` sends the stroke to its argument (a literal, so it is spliced) and the rest to `e`.

Creating a value with a library function is the same thing with nothing to wrap, e.g. insert `circle default default default` and unravel through `🧶circle`.

A function that can be lashed on is marked with `@lasher`. The expression being wrapped is its last argument (the one `|>` feeds).

```knot
@lasher
@unravel(🧶set_stroke)
set_stroke :: Color -> Node -> Node
```

The engine picks the first lasher whose rule, given the request, returns a request for the last argument that already `fits` the current value of `e`. If part of the request is still unmet it repeats with what remains. The application can also name the function to lash.

By default a lash goes at the very end, on the outermost expression that produces the value. A `@cleat` on an expression moves it there

```knot
circle 120 80 30 |> @cleat set_fill tomato |> rotate 30
```

FUTURE named cleats e.g. `@cleat("style")` so that a request can aim at one of several
FUTURE named lashers e.g. `@lasher("meowmeow")` so that a request can aim at a specific lasher, although is this actually useful in practice?

### Unravel strategy

The application sends a strategy along with its request:

- unravel first (default): unravel all the way back. If that snags, lash at the nearest cleat that was passed, or at the very end
- lash first: at the first cleat reached try the lashers. If none takes the request, carry on unravelling
- splice here: replace the expression the request lands on

Lash first can leave 2 setters behind, e.g. with `set_fill red` inside the cleat, asking for blue adds a `set_fill blue` at the cleat and keeps the red one. A setter outside the cleat is reached first and is edited as usual.

TODO what is "Splice here" do we need/want that?
TODO what are other unravel startegies we might want? e.g. no lash strategy, no bubbling up splices in reda onyl libraries, etc


### Braching and Converging

A request may eventually "branch" and "converge" producing 2 potentially conflicting requests. A conservative reconciliation algorithm is as follows:

On a given expression if the engine sees 2 or more result requests:

1. check if they conflict (see below)
2. pick one Constriction to satisfy first producing a new value at the merge node
3. "push forward" the value through the unravel chain at that node evaluating until we hit a branch node, refreshing affected values.
4. unravel using the changed input as a "pinned" value and see if the output Constriction is still satisfied
5. if not, repeat 3-4 from this node until a program output node is reached; fail if its request is still unsatisfied.

Keep all requests and explicit locks. Stop on no progress or exhausted shared budgets,
roll back failed attempts, and apply only after every request and pin passes final validation.

A failed candidate does not prove the requests conflict. For example, ranges
`[0, 10]` and `[5, 15]` overlap even if materialization chooses 0 and 15.
Checking a candidate against all requests proves success, but its rejection does
not prove impossibility. Arbitrary `fits` predicates provide no general conflict test.

Better rules for producing more liekly shared candidates can be added in the future, the above is sufficient for now.


### Editable Nodes and Source Edits (creation and replacement boundaries)

Replacement/creation boundaries are hardcoded in the engine:

- it is a primitive 
- it is an atomic expression from a read only library with primitive type (TODO maybe drop the primitive type requirement and replace with request being source constructible?)
- it is a non-atomic expression from a engine-coded list of permitted expressions (e.g. a list ctor [...])

all replaceable expressions are currently hardcoded at the engine level

FUTURE User specificed rules for replacing non-atomic expressions and replacing with non-atomic expressions is deferred

FUTURE User or Caller rules for replacing with wrapped expressions is deferred (e.g. if we request n'th element of [1..100] to x could become `[1..100] & ix n .~ x` granted no normal unravel possible here, sometimes a normal unravel is premitted but we still want this "override" style which could be specified in the unravelling output handler in code or as a parameter somewhere in the application's unravelling handle)

### List creation example

```knot
program = [a, b, Circle 1 2 3]
```

Requesting a changed second element, changed radius and one appended circle:

```knot
Prepend Nothing
  (Prepend (Just (Circle 10 20 30))
    (Prepend (Just (Circle 1 2 9))
      (Prepend (Just (Circle 4 5 6)) ExactEnd)))
```

In order: 

- Leave `a` unconstrained; retain its reference.
- Unravel through `b`'s computation; its reference can remain while its upstream definition changes.
- Unravel the existing constructor's arguments to change `3` to `9`.
- At the empty tail, the request requires another element. Materialize the missing suffix;
  the engine reconstructs the list, preserving existing source expressions.

The result is equivalent to `[a, b, Circle 1 2 9, Circle 4 5 6]`, plus upstream
edits to `b`. 

The empty tail may have no separate source span in bracket-list syntax. The engine
can replace the enclosing list expression while retaining the existing element expressions.


## 4. Higher-order functions

```knot
type alias Unraveller a b sIn sOut =
  { forward : a -> b
  , unravel : Feed a -> sOut -> Unravelled sIn
  }
```

A higher-order rule receives an `Unraveller` for each "callback" instead of a `Feed`.
These packages come first, in "callback" argument order, followed by feeds for the
ordinary arguments and the output request. Callback code stays fixed, and so do arguments
already supplied to the callback (see §9). The attached rule's signature specifies the "callback" request types;
`a -> b` alone cannot determine them.
For example, the default `map` unravel for lists has the following signature

```knot
🧶mapList :: Constriction sIn a =>
  Unraveller a b sIn sOut -> Feed (List a) ->
  ListConstriction sOut -> Unravelled (ListConstriction sIn)
```

A callback's unravel is only called by the rule it was passed to, so that rule decides what a `Splice` from the callback means. Passing it on splices the whole higher-order call (e.g. all of `map f xs`). The default `map` rules turn it into a `Snag`.

`map` keeps the shape of the list. Growing a list through a callback is a different policy, so it is a different function: `growMap` uses `Default` (§1) to create the new input.

Multiple arguments are supported.

```
type alias Unraveller2 a b c sA sB sOut =
  { forward : a -> b -> c
  , unravel : (Feed a, Feed b) -> sOut -> Unravelled (sA, sB)
  }

```

AGENT NOTES BELOW REDUNDANT CAN DELETE
- each input has its own pin
- `Snag` fails the whole call, `Unravel (requestA, requestB)` proposes both requests together
- which side changes is up to the rule, informed by pins. The engine does not favor one side
- the tuples are lazy, an input the rule does not look at is not forced
- `a -> b -> c` is one call with 2 arguments, `(a, b) -> c` is one call with 1 tuple argument. These are different contracts and are not interchangeable
- for 3 or more arguments extend the tuples in argument order

### Generated callback unravels

A function with no rule of its own (e.g. a lambda passed to `map`) gets a rule generated from its body.
The generated rule only makes exact requests, so its input request type is `Maybe a` and it needs `Eq a`. This is the `sIn` a higher-order rule sees for such a callback.

Nested higher-order callbacks extend naturally with recursion. The example below
is illustrative; support is deferred for simplicity.

```
-- Unraveller for type (a -> b) -> c -> d
type alias HigherUnraveller a b c d sA sB sC sD =
  { forward : (a -> b) -> c -> d
  , unravel :
      Unraveller a b sA sB ->
      Feed c -> sD -> Unravelled sC
  }
```

## 5. Sensitivity and adapters

TODO rename to Tension or and TensionConversion, the only thing I don't like about "Tension" is it doesn't describe the binary nature of the Constriction... So maybe we need something better (you could do `TautOrNot` or just Taut)

Applications default to the compiler-generated `Sensitivity T`; unravels may use
other constriction types. Its expansion is:

```text
Sensitivity { f : T, ... } = { f : Sensitivity T, ... }
Sensitivity (T1, ..., Tn)  = (Sensitivity T1, ..., Sensitivity Tn)
Sensitivity (List T)      = ListConstriction (Sensitivity T)
Sensitivity primitive    = Maybe primitive
```

ADTs generate one request variant per value constructor, qualified by the sensitivity
type, with each field replaced by its sensitivity type. For example:

```knot
type Shape = Circle Float Float Float | Rect Float Float
-- Requests a Circle with radius 6; other fields are unconstrained:
Sensitivity.Shape.Circle Nothing Nothing (Just 6)
```

Note, recursive types generate recursive sensitivity types, rather than infinitely expanding:

```knot
type Box = Box Box | Empty
-- Sensitivity Box = Sensitivity.Box.Box (Sensitivity Box) | Sensitivity.Box.Empty
```

Derivation fails for function or opaque types, including fields containing them.

`Sensitivity` is an optional shared conversion representation. Conversions may
narrow a request; they need not represent every constriction losslessly.

```knot
interface ToSensitivity s t where
  toSensitivity :: s -> Maybe (Sensitivity t)
interface FromSensitivity s t where
  fromSensitivity :: Sensitivity t -> Maybe s
```

Use declared conversions to sensitivity and, if needed, from sensitivity to the
selected rule's request type. No arbitrary conversion chains or instance synthesis.

**Conversion law:** if conversion from request `r` succeeds with `r'`, then
`fits r' value` must imply `fits r value`. Narrowing is allowed; widening is not.
The law applies in both directions. Runtime validation checks both requests for
the actual candidate; it cannot prove the law universally. Adapter instances must
obey it; ordinary `Constriction` instances need not provide adapters.
Round-trip identity is not required, since narrowing is allowed.


## 7. Standard Constricitons and helpers

```knot
-- only one fitting value
instance Eq t => Constriction t t

-- one fitting value or "don't care"
instance Eq t => Constriction (Maybe t) t

-- list of fitting values
instance Eq t => Constriction (List t) t

-- see below
type ListConstriction s = AnyRemainder | ExactEnd | Prepend s (ListConstriction s)
instance Constriction s t => Constriction (ListConstriction s) (List t)

```

`AnyRemainder` accepts any tail without forcing it. `ExactEnd` requires `[]`.
`Prepend s rest` requires a head fitting `s`, then checks the tail.
`Prepend Nothing (Prepend (Just 7) AnyRemainder)` constrains only the second element.

```knot
-- a request, or no request at all
type FieldConstriction s = NoRequest | RequestField s
instance Constriction s a => Constriction (FieldConstriction s) a
```

`FieldConstriction` is for when a constriction has no "don't care" case of its own.
`NoRequest` accepts any value and skips the input without forcing it. `RequestField s` routes `s` as usual.
`materialize` gives `Nothing` for `NoRequest`.
Skipping an input does not waive its pin, branch guards or final output validation.


## 8. Whipping (Unravelling Heads)

By default, most runtimes will provide a default interfaces based on the program output so that the program can be unravelled based on those objects.

We also want to allow such interafces to be manually declared in the program. A `@whip` annotation uses a "whipping" function to tie a variable of concrete non-function type to be unravelled to an "output object" which is application defined. 
The application will interpret this as it desires, in most cases binding the unravelling UI to some object in the program output or to some UI at the application scope

e.g. 
```
@whip(object_size_dragger wheel)
my_radius = 5

@whip(object_center_dragger "x" wheel)
some_number = 4

@whip(object_center_dragger "y" wheel)
my_expression = some_number + 1

wheel = Circle some_number my_expression my_radius

program = [wheel]
```

(this example essentially illustrates how to reimplement what a reasonable default applicatio unravelling behavior for Circle might be)

- several whips on one expression are allowed
- the application defines the controls, where they are anchored, their UI and how they behave across program revisions
- a whip previews its edit live like any other edit

e.g. the drawing library has `IntSlider` and `ScaleHandle` controls and `OnNode` / `OnNodes` anchors:

```knot
@whip(OnNodes decorations (ScaleHandle "Shape scale"))
shapeScale :: Float = 1.0
```

Whips are implemented by the application. The engine only parses the annotation.

A whip's control is data the application reads, so it can also say which unravel strategy (§3) to use, e.g.

```knot
@whip(LashFirst (ColourPicker "Outline"))
outline :: Color = tomato
```


# 9. Unsupported Features

## Unravelling functional types

e.g. 

```
f x y = x + y
```

unravelling the expression

```
map (f 5) nums  
```

has no way of touching the expression `f 5 :: Int -> Int` right now 