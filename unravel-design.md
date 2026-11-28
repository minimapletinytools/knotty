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
some_valid_unravel_for_f :: a -> sOut -> Unravelled sIn
```

similarly

```
g :: a -> b -> out
some_valid_unravel_for_g :: (a, b) -> sOut -> Unravelled (sA, sB)
-- or, when the rule wants to know which inputs are pinned (see below)
some_valid_unravel_for_g :: (Feed a, Feed b) -> sOut -> Unravelled (sA, sB)
```

Requires `Constriction sA a`, `Constriction sB b` and `Constriction sOut out`.

The rule is given the current value of each input.

The result type

```knot
type Unravelled s = Unravel s | Splice | Snag
```

- `Unravel s` requests `s` of the inputs
- `Splice` replaces the expression the rule was called on (the whole call, including its arguments). The engine materializes the rule's output request and prints that value as source. If the request does not materialize the edit fails
- `Snag` fails the edit

`Unravelled` is a monad, `Splice` and `Snag` both end a `do` block.


### Pins

An input is "pinned" when it can no longer change in this edit: every source value it depends on has already been decided (§3 Branching and Converging), or is locked or read only.

A rule with several inputs can ask to see the pins by taking a `Feed` for each input in place of the plain value

```knot
type alias Feed a = { value : a, pinned : Maybe a }
```

`value` is the current input value. If `pinned = Just p` the input is pinned at `p`, and the rule should send its request to the other inputs.

- pins are optional. A rule that takes plain values works the same, it just can not route around a pinned input
- the engine enforces pins either way. If a rule requests `s` of a pinned input then `fits s p` must hold, otherwise it is a snag
- a rule with one input never takes a `Feed`. If its only input is pinned nothing can change, so the engine does not call the rule: the request must already fit
- TODO the pinned type should be Sensitivy type rather than Maybe for more granularity

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


A function that can be lashed on is annotated with `@lasher`. The type being lashed onto is its last argument (the one `|>` feeds) which must match the result type.

```knot
@lasher
@unravel(🧶set_stroke)
set_stroke :: Color -> Node -> Node
```

The engine tries the lashers in the order they are declared and keeps the first one that the request goes through. If part of the request is still unmet it repeats with what remains.

By default a lash goes at the very end, on the outermost expression that produces the value. A `@cleat` flags an expression that produces the value as the place to lash onto instead. The lash goes right after the cleat and the expression inside the cleat is not changed. e.g. asking for a blue fill

```knot
@cleat (circle 120 80 30 |> set_fill tomato) |> rotate 30
-- becomes
@cleat (circle 120 80 30 |> set_fill tomato) |> set_fill blue |> rotate 30
```

- a setter after the cleat is reached before the cleat, so asking for green next edits the `set_fill blue`. Lashings do not pile up behind a cleat
- for the same reason a setter after the cleat is never protected, e.g. in `@cleat (e) |> set_fill paint` asking for blue changes `paint`. Put it inside the cleat to keep it
- cleats can be nested, the outermost one on the way back is the one used
- a cleat in a definition the value comes from counts, e.g. `base = @cleat (circle 120 80 30)` and `program = [base |> rotate 30]`

`@cleat` can also go on a definition. Then every use of it is a cleat, i.e. `f a b` is the same as `@cleat (f a b)` wherever it is called. For example, prelude defines

```knot
@cleat
cleat :: a -> a
cleat x = x
```

so `e |> cleat` is the same as `@cleat (e)` and the lash goes right after it, e.g. `e |> cleat |> set_fill blue`.

- the body of the function is not looked at for this, the cleat is the call
- `@cleat` on a definition with no parameters makes each place it is used a cleat, e.g. `@cleat` on `base = circle 120 80 30` gives `base |> set_fill blue |> rotate 30`. Compare with `base = @cleat (circle 120 80 30)` where the lash goes in the definition
- a cleat in read-only source is ignored since nothing can be written after it, the lash goes to the next cleat out or the very end. Unlike a splice a lash does not bubble up

`@belay` on a definition flags your own code as read-only. Nothing inside will be edited during unravels--like read only libraries. So splices bubble up out of it and cleats inside it are ignored. 

- a belayed definition with no parameters, e.g. `@belay` on `shared = 1`, is replaced where it is used
- a literal inside a belayed function is a splice of the whole call, not a snag, so asking for a new fill above does not lash a `set_fill` after the call

TODO should a splice that can not be printed (partial request) turn into a snag so that it can lash at the call?

Creating a value with a library function is similar. Only the result type of the lasher must match type of the expression being created.  e.g.

```knot
@lasher
@unravel(🧶circle)
circle :: Float -> Float -> Float -> Node
```

the engine creates `circle default default default` if possible and then unravels onto that expression to produce the final source candidate.

- every argument type needs a `Default`, otherwise that lasher can not be used to create
- the engine picks the first lasher whose rule answers `Unravel` for the request, e.g. for a circle request `🧶rect` snags and `🧶circle` answers
- a lasher only changes how the new value is written. If the value could not be added as data (e.g. the list does not accept new elements) then no lasher is tried
- if no lasher takes the request the value is written as data


FUTURE named cleats e.g. `@cleat("style")` so that a request can aim at one of several
FUTURE named lashers e.g. `@lasher("meowmeow")` so that a request can aim at a specific lasher, although is this actually useful in practice?

### Unravel strategy

The application sends a strategy along with its request:

- unravel first (default): unravel all the way back. If that snags, lash after the first cleat that was passed (the outermost one), or at the very end if there is none
- lash first: at the first cleat reached (or at the very end if there is none) try the lashers. A lasher is only kept if its own arguments are the only thing that changes. If none takes the request, unravel as usual
- splice here: replace the expression the request lands on

Lash first leaves 2 setters behind, e.g. with `set_fill red` inside the cleat, asking for blue adds a `set_fill blue` after the cleat and keeps the red one. A setter outside the cleat is reached first and is edited as usual, so the next request edits the `set_fill blue`.

If lash first finds no cleat it writes one, e.g. `e |> set_fill paint` becomes `e |> set_fill paint |> cleat |> set_fill blue`. The next request edits the `set_fill blue`.

TODO what is "Splice here" do we need/want that?
TODO what are other unravel startegies we might want? e.g. no lash strategy, no bubbling up splices in reda onyl libraries, etc


### Braching and Converging

A request may "branch" (a rule with several inputs sends a request to each) and later "converge" (2 branches reach the same source value, e.g. a variable that is used twice). The 2 branches may ask that value for different things.

e.g.
```knot
base = 10
program = (base * 2, base + 5)
-- request: (Just 22, Just 20)
```

the first branch wants `base = 11`, the second wants `base = 15`. `base = 11` with the `5` changed to `9` satisfies both.

The engine unravels in passes. Each pass decides one source value:

1. evaluate the program with the edits made so far ("push forward"). If the output fits the request we are done
2. unravel from the output using the current values. A request that already fits stops there
3. the first source value that needs to change is changed and frozen. Go to 1
4. if a request reaches a frozen value that it does not fit, the pass fails

An input whose source values are all frozen is pinned (§2 Pins), so a rule with several inputs can send its request the other way.

In the example
- pass 1: the first branch sets `base = 11` and freezes it
- pass 2: the first branch fits. For `base + 5` to be `20`, `+` sees its left input pinned at `11` and requests `9` of the `5`
- pass 3: the output fits

Every rule always runs on fresh values because each pass starts again from the output. There is no need to track which branch a value came from.

If a pass fails the engine tries once more deciding values in the other order.

This is not complete:
- a frozen value is never revisited. `base * 2 + base` requested to be `36` needs `base = 12`, but going left first gives `13` and going right first gives `16`, and both fail. Only a rule for the whole expression can solve this
- a failed candidate does not prove the requests conflict. For example, ranges `[0, 10]` and `[5, 15]` overlap even if materialization chooses 0 and 15. Arbitrary `fits` predicates provide no general conflict test

Any edit that is accepted is correct, the output is always checked against the request at the end.

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