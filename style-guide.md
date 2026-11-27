# Knot naming and style

How Knot library code is named. The rule of thumb: write it the way Haskell
would, then apply the Knot conventions below on top.

This covers the bundled libraries (`Prelude`, `String`, `Map`), the drawing
library and the examples. There has been no release, so names that break these
rules should be fixed, not kept for compatibility.

## Follow Haskell first

- Functions and values are `camelCase`. Types, constructors, interfaces and
  modules are `PascalCase`.
- Prefer the name Haskell already uses. `String` follows `Data.Text`; `Map`
  follows `Data.Map`.
- Knot departs from Haskell in a few deliberate places:

| Knot | Haskell | Why |
| --- | --- | --- |
| `map` | `fmap` | One name for mapping over any functor |
| `empty` | `mempty` | Shorter, same meaning |
| `Result e a`, `Ok`, `Err` | `Either e a`, `Right`, `Left` | Says which side is the failure |
| `List a` | `[a]` | Types are always written as applications |
| `\|>` and `\|>>` | `&` and `>>>` | Pipe and forward composition; there is no `.` or `$` |
| `interface` | `class` | |

`>>` is the monadic "then", as in Haskell.

## Unravel rules

A function that can be attached with `@unravel` starts with 🧶. Nothing else
does.

| The rule is for | Its name | Examples |
| --- | --- | --- |
| A named function | 🧶 plus the function's name, in the same casing | `🧶not`, `🧶fst`, `🧶growMap`, `🧶set_fill` |
| An operator or interface method on one type | 🧶 plus the method's word and the type, as GHC names its specialised functions | `🧶plusFloat`, `🧶timesFloat`, `🧶negateFloat`, `🧶appendList`, `🧶mapList` |
| A method shared by several instances | The same, with the interface in place of the type | `🧶plusIntegral`, `🧶timesIntegral` |
| A variant that takes the callback's rule as an argument | The rule's name plus `With` | `🧶mapWith`, `🧶concatMapListWith` |

- One rule may serve several functions when they are the same operation:
  `🧶id` is attached to `id` and to `Integer`'s `fromInteger`.
- Helpers that build rules but are not rules themselves take ordinary names,
  with an `unravel` prefix where it helps: `unravelUnary`, `unravelBinary`.
- A module that holds a library's rules is named `...Unravel`
  (`DrawingUnravel`).
- A callback's rule passed as an argument is called `rule` or `ruleF`.

## What a rule answers

A rule returns `Unravelled s`. The type is the past participle; its
constructors are what to do next.

| Answer | Meaning |
| --- | --- |
| `Unravel s` | Ask `s` of the inputs |
| `Splice` | Replace the call itself with the printed, materialized request |
| `Snag` | Fail the edit |

- Write a rule's answers natively. Where the working-out is naturally in
  `Maybe`, finish with `orSnag`, which turns `Nothing` into `Snag`.
- A rule that takes a callback decides what the callback's `Splice` means.
  `snagOnSplice` refuses it.
- The record a higher-order rule receives for a callback is an `Unraveller`,
  spelled with two l's like `Unravelled` and "unravelling".

## Constrictions and requests

These are two different things, and the names keep them apart.

- A **constriction** is a type that describes which values are acceptable. It
  has an instance of the `Constriction` interface. Name these types
  `...Constriction`: `ListConstriction`, `NodeConstriction`,
  `RectConstriction`, `FieldConstriction`.
- A **request** is a role, not a type: a constriction value travelling
  backwards through the program as a demand. Use "request" for values:
  parameter names (`request`), constructors (`RequestRect`, `NoRequest`) and
  helpers (`splitListRequest`).
- A **pin** is the same kind of value in a different role: a limit that stays
  on an input and says what it may become (`pinned`, `pinAllows`).

So a `Maybe Float` can be a request in one place and a pin in another. That is
why the type is never named after the role.

A module that holds a library's constriction types is named `Constrictions`.

## Defaults

`Default` gives the value to start from when one has to be created and there
is no current value to adjust.

- Built-in types have defaults: zero, `False`, `""`, `[]`, `Nothing`.
- Records, tuples and unit take theirs from their parts.
- Any other type declares `instance Default`. There is no derived default, so
  the author of a sum type picks the constructor.

## Words to avoid

| Don't write | Write | Note |
| --- | --- | --- |
| constraint | constriction | For the unravel idea. The type checker's own "constraint", meaning an interface requirement such as `Eq a =>`, is a different thing and keeps its name. |
| `...Back`, `back` | 🧶 for a rule, `rule` for a rule passed as an argument, `...Unravel` for a module | |
| `...Request` as a type name | `...Constriction` | "Request" is for values |
| seed | default | For the starting value of something being created |
| replace, fail (as a rule's answer) | splice, snag | |
| `Unraveler` (one l) | `Unraveller` | |

## Words held in reserve

Don't use these for anything yet. They are set aside for ideas that are not
settled.

| Word | Possible use |
| --- | --- |
| tension, slack, taut | Evaluation order, or whether a field is under demand |
| tug | A rope-themed replacement for "request" |
| stock | `@stock`, a per-function override of `Default` |
| lash, `@lasher` | Adding a call around an expression, and the mark on a function that may be added |
| cleat, `@cleat` | The place in an expression where lashings attach |

## Annotations

| Annotation | On | Meaning |
| --- | --- | --- |
| `@unravel(🧶rule)` | A function or instance method | The rule that unravels it |
| `@whip(control)` | A top-level binding | A control the application binds to that value |
| `@position(x, y)` | An expression or definition | Where its knot sits in Tangle |
| `@story("Group")` | A definition | Lists it in the Tangle storybook |

Whip controls are named for what the control is (`IntSlider`, `ScaleHandle`),
and their anchors for where it appears (`OnNode`, `OnNodes`).

## Tangle vocabulary

| Term | Meaning |
| --- | --- |
| Knot | A node |
| Rope | A connection between two eyelets |
| Eyelet | A dot a rope attaches to |
| Bobbin | A pill that names a value: a parameter, a pattern, a `let` or `<-` binding |
| Slub | A pill for a function on a chain |
| Band | One statement of a `do` block |

## Not yet settled

- **Casing in the drawing library.** It is `snake_case` (`set_fill`,
  `rounded_rect`) while everything else is `camelCase`. The rule above says
  `camelCase`; the library has not been converted.
- **Variants of one function with a different unravel.** `growMap` is the only
  one. A scheme is needed before there are more: a prefix (`growMap`) or a
  suffix (`mapGrowing`).
- **`Map` against `Data.Map`.** `get`, `remove`, `isEmpty` and `values` differ
  from `lookup`, `delete`, `null` and `elems`, and the balancing internals are
  exported.
- **One `Feed` alias.** `{ value : a, pinned : Maybe a }` is written out in
  full in the Prelude and aliased as `Feed` elsewhere.
