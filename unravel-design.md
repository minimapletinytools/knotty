# Unravel design

Intended design · 2026-11-05. Implementation migrations are tracked in
[follow-ups](unravel-design-followups.md).

Unravel rules translate output requests into input requests. The engine turns these into source changes and validates the output.

## 1. Constraints and materialization

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
some_valid_unravel_for_f :: Feed a -> sOut -> Maybe sIn
```

similarly

```
g :: a -> b -> out
some_valid_unravel_for_g :: (Feed a, Feed b) -> sOut -> Maybe (sA, sB)
```

Requires `Constriction sA a`, `Constriction sB b` and `Constriction sOut out`.

An outer `Nothing` in the result means failure. 


The type

```knot
type alias Feed a = { value : a, pinned : Maybe a }
```

describes the current input value, refreshed after speculative edits.
If `pinned = Just p`, the returned input request `s` must satisfy `fits s p`.
The engine must also preserve the actual pinned input. No condition on
`materialize s` is needed: it may choose another acceptable value or return `Nothing`.

Attach rules using `@unravel(rule)` on functions or data constructors.

### Unravel selection

The unravel function to be used is chosen in the following order

1. Prefer rules accepting the output request type directly, including polymorphic specialization.
2. Otherwise use declared `ToSensitivity` / `FromSensitivity` conversions (§5).
3. Break ties within each group by annotation order. No ranking by input matches.
4. If the selected rule returns `Nothing`, fail; do not try another rule.


## 3. Unravelling

Given a "request" a basic unravel is as follows:

1. Retain the request for final validation. If already satisfied, leave its source.
2. Otherwise, run the selected unravel through the source tree.
3. At a permitted replacement/creation boundary, call `materialize`, check `fits`,
   and construct source. Preserve surrounding expressions.
4. Recheck and evaluate the candidate program. Publish one atomic edit only if
   every request and retained pin/branch obligation passes. Apply/undo check revisions.

Boundaries include literals, editable uses of read-only values, and structural
replacement such as a missing list suffix or constructor change. Imported
library definitions are read-only. 

List insertion can preserve existing expressions through constructor-context recipes.
This does not give rules a general operation for wrapping existing source in an
arbitrary function call; that remains deferred.

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
type alias Unraveler a b sIn sOut =
  { forward : a -> b
  , unravel : Feed a -> sOut -> Maybe sIn
  }
```

A higher-order rule receives an `Unraveler` for each "callback" instead of a `Feed`.
These packages come first, in "callback" argument order, followed by feeds for the
ordinary arguments and the output request. Callback code stays fixed; supplied
arguments are not automatically pinned. The attached rule's signature specifies the "callback" request types;
`a -> b` alone cannot determine them.
For example, the default `map` unravel has the following signature

```knot
unravel_map :: Constriction sIn a =>
  Unraveler a b sIn sOut -> Feed (List a) ->
  ListConstraint sOut -> Maybe (ListConstraint sIn)
```

Multiple arguments are supported.

```
type alias Unraveler2 a b c sA sB sOut =
  { forward : a -> b -> c
  , unravel : (Feed a, Feed b) -> sOut -> Maybe (sA, sB)
  }

```

Nested higher-order callbacks extend naturally with recursion. The example below
is illustrative; support is deferred for simplicity.

```
-- Unraveller for type (a -> b) -> c -> d
type alias HigherUnraveler a b c d sA sB sC sD =
  { forward : (a -> b) -> c -> d
  , unravel :
      Unraveler a b sA sB ->
      Feed c -> sD -> Maybe sC
  }
```

## 5. Sensitivity and adapters

TODO rename to Tension or and TensionConversion, the only thing I don't like about "Tension" is it doesn't describe the binary nature of the Constriction... So maybe we need something better (you could do `TautOrNot` or just Taut)

Applications default to the compiler-generated `Sensitivity T`; unravels may use
other constriction types. Its expansion is:

```text
Sensitivity { f : T, ... } = { f : Sensitivity T, ... }
Sensitivity (T1, ..., Tn)  = (Sensitivity T1, ..., Sensitivity Tn)
Sensitivity (List T)      = ListConstraint (Sensitivity T)
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
type ListConstraint s = AnyRemainder | ExactEnd | Prepend s (ListConstraint s)
instance Constriction s t => Constriction (ListConstraint s) (List t)

```

`AnyRemainder` accepts any tail without forcing it. `ExactEnd` requires `[]`.
`Prepend s rest` requires a head fitting `s`, then checks the tail.
`Prepend Nothing (Prepend (Just 7) AnyRemainder)` constrains only the second element.


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