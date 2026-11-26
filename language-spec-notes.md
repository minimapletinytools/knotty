# Knot Language Spec — v0.1

Knot is a lazy, pure functional language for Tangle's editable node graphs.
Twine evaluates it. This document records the language rules and marks future
work explicitly; [knot_v0.md](knot_v0.md) tracks implementation scope.
“Matches Haskell/Elm” applies to the named feature, with Knot's exceptions below.

## 1. Design Principles

- **Lazy, call-by-need evaluation:** matches Haskell, including recursive and
  infinite structures.
- **Pure functions and immutable values:** effects are described with `IO`.
- **Partial functions allowed:** `error`, `undefined`, and incomplete matches.
  Case coverage problems are warnings, not compile errors.
- **Graph representation matters:** preserve source constructs and their origins
  so Tangle can edit them (§3).

## 2. Relationship to Haskell & Elm

Knot represents a middle ground between Haskell and Elm, tailored specifically to visual node graphs.

### 2.1 From Elm
* **Record Syntax**: `{ field = value }` for construction, `{ record | field = value }` for updates, dot access without spaces (`record.field`), and `{ x, y }` shorthand destructuring in patterns (§8.1).
* **Type Declarations**: ADTs are defined using the `type` keyword instead of `data`, and aliases use `type alias`.
* **Module & Import System**: Every file is a module, imports are qualified by default, and namespace control is managed via the `exposing` keyword.
* **No `where` clauses**: All local scope bindings must use `let...in` (matching Elm's let-only scoping).
* **Pipe Operators**: Forward pipe (`|>`) and forward composition (`|>>`) replace `.` and `$`.
* **No `.` or `$` Operators**: Haskell's backward composition (`.`) and low-precedence right-associative application (`$`) are both omitted — `|>`/`|>>` cover chaining and composition, and parentheses handle the rest.
* **No Map Literals**: Maps are constructed using `Map.fromList` (matching Elm's `Dict.fromList`).
* **Simplified Imports**: No `qualified` or `hiding` keywords (matching Elm's import design).
* **Pattern Aliases (`as`)**: Pattern aliases use `as` instead of Haskell's `@` to prevent syntactic conflicts with layout annotations.
* **No Guards**: Pattern matching does not support guards, matching Elm's simpler `case` expressions.
* **no Partial Sections**: `(+)` supported but not `(+2)` `(2+)`

### 2.2 From Haskell
* **Lazy evaluation (call-by-need)**: Values are computed only when demanded (unlike Elm's strict evaluation). This allows for infinite structures and deferred graph computation.
* **Built-in typeclasses**: Overloaded operations use a typeclass-like interface system — the closed interface list is in the prelude (§12).
* **Type Signatures**: Standard `::` syntax.
* **List cons operator**: list cons (`:`) preserved.
* **Monadic/List Operators**: `do` syntax and bind operator (`>>=`) preserved — `(>>=)` and `pure` are interface methods (§12).
* **Holes**: `_` can be used in place of an expression as a typed hole (§17). The named form `_somename` is allowed only where a value is discarded (patterns and `let` left-hand sides, §17.2), never as an expression placeholder.
* **User-Defined Operators**

### 2.3 Omitted from Haskell/Elm
* **No user-defined typeclasses**: Unlike Haskell's typeclasses (or Elm's extensible record polymorphism), the *set* of interfaces in v0 is closed — fixed by the prelude (§12) — to keep compile-time dictionary passing and type checking simple; users cannot declare a brand-new interface. Instances of these existing interfaces are open, though — see §2.4 and §10.
* **No List Comprehensions**: Haskell's list comprehension syntax (`[x | x <- xs]`) is omitted in favor of standard map/filter functions.
* **Right to Left functor operators `<|` `<<` `=<<`**: not supported
* **No `.` function composition operator**
* **No multi-clause function definitions**: Unlike Haskell's `f 0 = ...` / `f n = ...` style, a function name may be bound by only one equation; branch on argument patterns using `case`, matching Elm.
* **Basically anything else that's in Haskell but not Elm that's not listed in 2.2**

### 2.4 Different / Custom
* **Open Instances, Closed Interfaces**: The *set* of interfaces is closed (no new ones), but instances of the existing interfaces are open — users may implement them for their own types, not just built-in primitives. The interfaces themselves, with their methods and superclasses, are declared in the prelude (§12).
* **Unravel System (Reverse Execution)**: Backwards execution solving rule annotations (`unravel`), allowing changes to flow in reverse through the graph.
* **Holes in LHS of let bindings**: `let _ = expression` is allowed and dropped by the compiler.

---

## 3. Node Graph Model

```text
source -> surface AST -> canonical AST -> typed AST -> executable core -> Twine
                         Tangle mapping                one-way lowering
```

Retain `do` (including ordered do-let statements), lists, patterns, bindings and
annotations through checking. In particular, `do` remains a Tangle sequencing
node even when it contains only a final expression. Desugar for execution only
in `knot-core`, retaining source origins; core is not the editable document.

Source/graph round trips must preserve bindings, captures, sharing, statement/arm
order and graph metadata. Formatting may normalize. Printing, comment preservation
and round-trip tooling remain future work. Visual grouping does not itself
introduce lexical scope or a lambda.

## 4. Lexical Syntax & Layout

### 4.1 Comments

Matches Haskell: `--` line comments and nested `{- ... -}` block comments.
Knot always treats `--` as a comment start, including inside a symbol run.

### 4.2 Identifiers & Keywords

Identifiers use ASCII letters, digits, `_`, `'` and Unicode `Other_Symbol`
characters (including emoji). Lowercase or emoji starts values, fields and type
variables: `🧶plusFloat` is an ordinary function name. Uppercase ASCII starts types,
constructors and modules. Emoji sequences may include variation selectors, skin
tones and joined symbols. Names are matched exactly, without normalization.
`_` / `_name` are separate hole/discard syntax (§17). Spans count UTF-8 bytes;
layout and diagnostic columns count Unicode scalars.

Operators are maximal runs of `! # % & * + - / : < = > ? ^ | ~`, except reserved
`=`, `|`, `->`, `<-`, `::`, `=>`. User `:`-leading operators are ordinary functions.

Reserved words: `module`, `exposing`, `import`, `as`, `type`, `alias`, `let`, `in`,
`if`, `then`, `else`, `case`, `of`, `do`, `deriving`, `interface`, `where`,
`instance`, `infixl`, `infixr`, `infix`. `True` and `False` are ordinary constructors.

### 4.3 Layout / Indentation Rule

Matches Elm's layout-only approach: declarations and items within `let`, `case`
and `do` blocks align at their block's reference column. No explicit-brace or
semicolon alternative; braces are records.

### 4.4 File & Module Structure

Each file declares one module. Imported `Geometry.Shapes` resolves to
`Geometry/Shapes.knot` under a module root, with a matching module header.
The explicitly selected entry file may have any filename (§14).

## 5. Type System

### 5.1 Primitive Types

| Type | Meaning |
| --- | --- |
| `Bool` | `True` / `False` |
| `Int` | Signed 64-bit integer, wrapping arithmetic |
| `Integer` | Arbitrary-precision integer |
| `Float` | IEEE 754 binary64 (Haskell's `Double`, not its `Float`) |
| `String` | Unicode scalar-value text, not a list (§5.7) |
| `Unit` / `()` | The same unit type, with value `()` |

Numeric behavior matches 64-bit GHC: floor `div`/`mod`, IEEE division and signed
zero, and correctly rounded exact-ratio conversions. Integral division or exact
ratios with zero denominators error; nonfinite Float-to-Integer/Ratio conversions
error. Unordered Float `compare` returns `GT`. Float rendering is pinned to GHC
9.6.2; transcendental results may vary with host math. See the
[runtime's numeric contract](compiler/twine/README.md#core-and-dictionaries).

### 5.2 Type Aliases

Matches Elm: `type alias Point = { x : Float, y : Float }` is transparent,
not a new nominal type. Aliases expand before checking.

### 5.3 Algebraic Data Types

Matches Elm's declaration syntax, with optional `deriving` (§10.6):

```knot
type Shape = Circle Float | Rectangle Float Float deriving (Eq, Ord, Show)
type Maybe a = Just a | Nothing
```

### 5.4 Records

Matches Elm for construction, update, access and row polymorphism:

```knot
p = { x = 1.0, y = 2.0 }
p2 = { p | y = 3.0 }
x = p.x
distance :: { r | x : Float, y : Float } -> Float
```

`{ x : Float, y : Float }` is closed; `{ r | ... }` is open. Shorthand record
patterns infer open rows (§8.1). No `.field` accessor-function syntax; use
`\record -> record.field`.

### 5.5 Tuples

Fixed-size heterogeneous tuples of any arity ≥ 2, e.g. `(Int, String, Bool, Float)`.
The same arities work in types, expressions and patterns, with automatic `Eq`,
`Ord` and `Show` when their elements support them. `()` is unit; `(x)` is grouping.
No variadic tuple polymorphism or tuple spreading.

### 5.6 Other Builtin Types

The Prelude supplies `List`, `Maybe`, `Result`, `Ordering`, `Ratio`, `IO`, etc.;
`Map` supplies `Map.Map k v` (§12).

Embedders can register nominal opaque types such as `Geometry.Mesh`, with pure
monomorphic host functions. Native values are immutable snapshots; Knot can store
and pass them normally and define explicit instances for them. They have no
implicit `Eq`/`Ord`/`Show`. See the [host contract](compiler/twine/host-types.md)
for supported boundary types and v0 restrictions.

### 5.7 Strings

Matches Haskell's `Data.Text` model, with a one-scalar `String` replacing `Char`:
`String.toList "hé"` is `["h", "é"]`. Indexes/lengths count Unicode code points,
not bytes or graphemes. `String.fromList` concatenates strings. Character predicates
return `False` for non-singletons; `ord` errors. `chr` rejects invalid scalars.

`show` matches Haskell string rendering, escaping **all non-ASCII** as decimal
escapes and using `\&` where needed. Rendered escapes need not parse as Knot
source (§7.1). Category predicates follow `Data.Char` with Unicode 16.0 tables;
case conversion uses full, context-free host Unicode mappings and may expand text.

`String.toInt` accepts `[+-]?[0-9]+`; `String.toFloat` accepts
`[+-]?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?`. Both consume the whole input with
no whitespace; invalid input returns `Nothing`. Int overflow returns `Nothing`;
Float overflow/underflow follows binary64. No hex, `NaN` or `Infinity` input.

Text operations live in [String.knot](compiler/knot-canonical/lib/String.knot),
following `Data.Text` names/behavior with the character substitution above.
`lines`, `words`, `unlines`, `unwords`, and `show` are in the Prelude.
A separate raw-byte type is future work.

## 6. Type Inference & Checking

### 6.1 Let-Polymorphism / Generalization

Matches Haskell/Elm's Hindley–Milner inference: named top-level/let bindings can
be generalized; lambda, case and do-bound parameters cannot. Pattern-let bindings
and recursive groups containing them are monomorphic. Compound constraints such
as `Eq (f a)` and open-row structural constraints cannot be generalized in v0.

### 6.2 Numeric-Literal Polymorphism & Defaulting

Matches Haskell: integer literals use `Num.fromInteger`; decimals use
`Fractional.fromRational`. Literals retain exact, unbounded values until instance
selection (`0.1` can be exactly `1/10` as `Ratio Integer`). Int conversion wraps;
there is no overflow warning yet.

Defaulting follows Haskell's numeric rule, using **Int then Float** instead of
Integer then Double. At least one constraint must be numeric; all others must be
numeric or `Eq`, `Ord`, `Show`, `Enum`.

Knot's monomorphism restriction is **type-based**, unlike
[Haskell's syntactic rule](https://www.haskell.org/onlinereport/haskell2010/haskellch4.html#sect4.5.5):
a group with no function-typed member keeps constrained variables shared until
later uses determine them; unresolved ones default at module end.

### 6.3 Ambiguous Constraints

Undetermined constraints that cannot default are errors, e.g. `show empty`.
A signature cannot constrain a variable absent from its type:
`f :: Eq b => Int -> Int` is invalid.

## 7. Expressions

### 7.1 Literals

Decimal/hex integers (`5`, `0xFF`), decimals/exponents (`3.14`, `1.5e10`), and
strings (`"hello"`). A decimal point requires digits on both sides.
String escapes: `\\`, `\"`, `\n`, `\t`, `\r`, `\u{XXXX}`. No multiline strings.
Numeric expression literals are overloaded (§6.2), unlike numeric patterns (§8.1).

### 7.2 Records, Lists, Tuples

Matches Elm's literal syntax: `{ x = 1 }`, `[1, 2]`, `(1, "a")`.
No map literal; use `Map.fromList [("apple", 1)]`.

### 7.3 Let, If/Else, Case

Matches Haskell for recursive `let ... in`, `if ... then ... else`,
`case ... of`, and `\x y -> body`. Parameters may be patterns.
Bindings in a let group are mutually visible; pattern-let projections are lazy.

A lambda/if/let/case/do can be an operator's final operand
(`xs |> \x -> x + 1`); as a function argument it needs parentheses.

### 7.4 Operators & Precedence

Matches Haskell's fixity rules: application binds tightest, precedence is 0–9,
and undeclared fixity is `infixl 9`. Equal-precedence operators must agree on
associativity; non-associative chains require parentheses.

Knot requires parentheses around symbolic names in declarations:

```knot
infixl 6 (+++)
(+++) a b = a + b
```

Fixity declarations are top-level, name a definition in the same module, and may
appear before or after its uses. Multiple names may share a declaration; duplicate
fixities are errors. **Imported definitions carry their fixities.**

Operators are ordinary functions, definable at top level or as instance methods,
not in `let`. Backticks make value names infix, including qualified names:
`` k `Map.member` m ``. Declare their fixity with backticks too:
`` infixl 7 `div`, `mod` ``. Constructors cannot be backticked.

### 7.5 Unary Negation

Matches Elm's spacing rule: `f -1` means `f (-1)`; `a - 1` and `a-1` are
subtraction. `f- 1` is a parse error. Negation uses `negate`.

### 7.6 Operator Sections

Matches Elm: `(+)` is a function reference. No `(+2)` or `(2+)` sections;
write `\x -> x + 2`.

## 8. Pattern Matching

### 8.1 Supported Patterns

Variables, `_` / `_name`, constructors, unit, pairs/triples, lists (`[]`, `[x]`,
`x : xs`), Int/String literals, Elm-style `as` aliases and `{ x, y }` record
shorthand. Record patterns ignore other fields and infer open rows; field
renaming/nested field patterns are unsupported.

Integer patterns are **Int-only**, not overloaded. No Integer, custom numeric or
Float literal patterns; use explicit comparisons. Refutable matches may fail at
runtime; lazy pattern-let bindings are checked when a bound component is demanded.

### 8.2 No Guards

Matches Elm: use `if ... then ... else` inside an arm.

### 8.3 Exhaustiveness Checking

Matches Haskell's warning approach for incomplete/unreachable case arms.
Equivalent diagnostics for function/lambda/do patterns are not complete in v0.

## 9. Function & Value Definitions

Haskell signatures and Elm's single-equation rule:

```knot
name :: Type
name arg1 arg2 = expr
```

Signatures are optional. Use `case` for multiple alternatives.

## 10. Interfaces (Typeclasses)

### 10.1 The Closed Interface Set

Haskell-style constraints and dictionary passing, but the interface set is fixed
by the [Prelude](compiler/knot-canonical/lib/Prelude.knot). Users add instances
and constrained signatures (`myMax :: Ord a => a -> a -> a`), not interfaces.

### 10.2 Instance Declarations & Coherence

Matches Haskell's instance syntax:

```knot
instance Eq Shape where
  (==) a b = ...
```

For one-parameter interfaces, v0 heads must be nominal constructors applied to
distinct type variables:
`Eq Shape`, `Eq (Box a)`, `Functor (Outcome e)`. Aliases expand first. No
specialized/nested/repeated arguments, bare variables, function heads, or context
variables absent from the head.

No custom record/tuple/unit instances. Use a nominal wrapper for custom behavior:
`type Point = Point { x : Float, y : Float }`.

For those interfaces, at most one instance per `(interface, nominal constructor)`
across the reachable module graph, including builtins. Orphans are allowed; overlaps are errors.
Methods are checked against the interface with the instance context available.
Missing methods need defaults; unknown/duplicate methods are errors.

Unravel annotations may attach to individual instance method definitions; their
meaning follows the selected instance. Primitive declarations carry no annotations.

### 10.3 Superclasses

Matches Haskell: `Ord` requires `Eq`, etc. The instance context and visible
instances must supply superclass evidence; matching a superclass head alone is
insufficient. Declaration order within a module does not matter.

### 10.4 Structural Automatic Derivation

Knot automatically supplies `Eq`, `Ord`, `Show` and `Default` for tuples, closed records and unit
when their components support them. `Default` gives the starting value for creating one
(`default :: a`): zero, `False`, `""`, `[]` and `Nothing` for built-in types, an explicit
instance for any other nominal type. These instances cannot be overridden. Open-row
obligations must close before resolution; they cannot be generalized in v0.

Records compare fields alphabetically and render as `{ x = 1, y = 2 }` (empty:
`{}`). Tuple/record formatting matches
[Elm](https://github.com/elm/core/blob/1.0.5/src/Elm/Kernel/Debug.js), with each
component using its own `Show` instance. ADT rendering/parenthesization matches
Haskell: `Just (Just (-1))`.

Knot's `showsPrec :: Int -> a -> String` returns text directly, not Haskell's
`ShowS`. An explicit `Show` instance must implement `show` or `showsPrec`;
each has a default in terms of the other.

### 10.5 Constructor Interfaces

Matches Haskell: interfaces such as `Functor` constrain type constructors;
partially applied nominal heads are supported. For example:
`countIt :: Foldable f => f Int -> Int`.

### 10.6 `deriving` for User ADTs

`type` declarations may derive `Eq`, `Ord`, `Show`, plus pointwise
`Semigroup`/`Monoid` for **single-constructor** types. Use `deriving Eq` or
`deriving (Eq, Ord, Show)`; never on `type alias`.

Contexts follow field requirements, including recursive fields; phantom parameters
need no constraint. Eq/Ord/Show follows Haskell's constructor/field ordering.
Unsupported field obligations (e.g. functions or unresolved `f a`) reject deriving.
No numeric or constructor-interface deriving. Ordinary coherence/superclass rules
apply: deriving `Ord` still needs `Eq`.

### 10.7 Two-Parameter Relations

Matches Haskell's multi-parameter classes, without functional dependencies or
instance-directed type inference. The closed Prelude includes:

```knot
interface Constriction s t where
  fits :: s -> t -> Bool
```

Builtins: `Eq t => Constriction t t` (exact), `Constriction (Maybe t) t`
(optional exact), and `Constriction (List t) t` (alternatives); the latter two also
require `Eq t`. Authors can add relations such as
`Constriction s t => Constriction (ListConstriction s) (List t)`.

The compiler also supplies fieldwise `Constriction` for equal-arity tuples and closed
records with exactly matching field names, recursively using component relations.
Identical constraint/value types keep exact equality and require `Eq`; no fallback
on a missing instance. Records validate alphabetically, tuples positionally, both
short-circuiting. Open rows must close; omitted fields are not wildcards.

Instance patterns may use variables and nominal applications in both positions.
The complete pair selects the instance; any potentially overlapping pair is
rejected, including across imports. Contexts must decrease in size and cannot
introduce variables or repeat them more often than the head. Custom structural
and higher-kinded relation patterns remain unsupported.

Signature contexts use bare variables (`Constriction s t => ...`). Both types must
be determined by the signature or use; neither determines the other. An unresolved
relation prevents numeric defaulting of its variables. Generalized inferred
relations may retain compound arguments. See the executable
[list-constraint example](corpus/semantic/valid/interfaces/list-constraint.knot).

## 11. Do-Notation

Matches Haskell's sequencing syntax:

```knot
do
  x <- someOption
  let y = x + 1
  pure y
```

Statements may bind with `<-`, discard an action's result, or introduce recursive
`let` bindings without `in`. End with an expression; `pure` is **not** inserted
implicitly. Sequencing lowers through `(>>=)`; a lone final expression remains
that expression. A failed do-pattern is a runtime match error, not `MonadFail`.
Tangle retains the original block (§3).

## 12. The Prelude

[Prelude.knot](compiler/knot-canonical/lib/Prelude.knot) defines the builtin types,
interfaces, methods/defaults, instances, values and fixities. It follows Haskell's
Prelude with Knot's adaptations; its actual signatures are authoritative.

Prelude names are implicitly in scope and may be shadowed; `Prelude.reverse`
still names the original. Import `Map` and `String` explicitly. There is no
`List.map` alias for Prelude's `map`.

Literals use `fromInteger`/`fromRational`; conditions use `Bool`; lists use
`List`/`:`/`[]`; negation uses `negate`; sequencing uses `(>>=)`; structural
instances/defaulting/deriving use the interfaces above. These refer to Prelude
identities, not unrelated local names.

Only trusted library modules may declare interfaces, primitives without bodies,
or opaque types without constructors. Host registration is the embedding
alternative (§5.6).

## 14. Modules & Imports

Matches Elm's module/import syntax. Local imports load, check and link end to end.

### 14.1 Module Declaration

```knot
module Geometry exposing (Point, distance, Shape(..))
```

`exposing (..)` exports all local names. `Shape` exports the type alone;
`Shape(..)` includes constructors. Re-exporting imported names is unsupported.

### 14.2 Imports

```knot
import Map as M
import String exposing (length, concat)
```

Imports enable qualified references; `exposing` additionally brings names into
unqualified scope (`(..)` means all exports). Exposed imports override Prelude
names. Operators use parentheses: `exposing ((+++))`. Ambiguous references error.

Instances are visible through transitive imports, independent of `exposing`.
Conflicts across the reachable graph error. Import cycles are unsupported.
Module roots, filename rules and remaining loader limits are in the
[module guide](compiler/knot-workbench/modules.md).

### 14.3 Entry Point

`tip` is Knot's `main`; running requires `IO Unit` (inferred or declared):

```knot
tip :: IO Unit
tip = pure ()
```

Matches Haskell's pure IO-description model: evaluating an action does not run it;
the host runner does. Running the same action twice repeats its effects.
`print`, `putStrLn`, and `getLine` are implemented.

## 15. Metadata & Annotations

Syntax and name resolution are implemented; annotation values are **not yet
checked or evaluated**. They do not affect forward execution. Typed keys and
graph-construction-time evaluation are future design, not current guarantees.

### 15.1 Stacked single-key form: `@name(args)`

Place above a definition/signature or let binding:

```knot
@nodeId("f1")
@position(100, 200)
f x = x + 1
```

One argument becomes the key's value; multiple arguments become a tuple.

### 15.2 Block form: `@{ ... }`

```knot
@{ nodeId = "f1", position = (100, 200), label = "Increment" }
f x = x + 1
```

Forms may mix. For ordinary metadata, block entries override single-key entries
regardless of order; among entries of the same form, the later one wins. Values
are expressions, but cannot themselves contain annotations.

The reverse-edit contract makes `unravel` an ordered, repeatable key (§16.1): all
entries contribute rules in source order. Callback companions are inferred from
forward and unravel signatures ([callback contracts](unravel-design.md#4-callback-companions-and-guards)). The optional,
repeatable `@callback_body(index, witness)` overrides inference with explicit
callback positions/contracts, checked when preparing an edit.
`@materialize(construction)` on a top-level selector declares a checked source
recipe for one concrete request/value type pair. Duplicate declarations reject;
all occurrences are retained for this check. See
[materializer declarations](compiler/knot-workbench/candidate-constructions.md).
Other keys retain their overwrite behavior.

### 15.3 Inline sub-expression annotations

Prefix annotations attach to the next **atom**, not the whole application:
`f @nodeId("n") x` annotates `x`. Use `@nodeId("n") (f x)` for the application.

### 15.4 Standard annotation keys

Proposed key types/meanings; not enforced in v0:

| Key | Type | Meaning |
| --- | --- | --- |
| `nodeId` | `String` | Intended persistent source-node identity |
| `position` | `(Float, Float)` | Canvas position |
| `label`, `doc`, `color`, `group` | `String` | Display/documentation/grouping metadata |
| `collapsed` | `Bool` | Initially collapsed |
| `unravel` | Pending §16 | Reverse-edit rule |

The key set is intended to remain open.

### 15.5 Annotation Typing & Scope

Keys are currently metadata names, **not ordinary function applications**.
How user-defined keys acquire types remains to be specified.

Values resolve in the enclosing lexical scope. An annotation above a function
cannot refer to that function's parameters; its lambda must bind its own inputs.
An inline annotation inside the body can reference parameters already in scope.

## 16. Unravel (Reverse Execution)

**Source editing implemented, with bounded traversal and solving.** The
[workbench API](compiler/knot-workbench/unravel.md) supports typed requests,
generated callback companions, materialization, revision checks and undo. The
[current design](unravel-design.md)
uses `Constriction s t` to validate a constraint `s` against a value `t`. Unravel
authors choose both input and output constraint types; no representation is
selected automatically.

### 16.1 What an unravel function is

A unravel rule receives input states and an output constraint, and proposes
an input constraint. For `f :: a -> b`, the rule shape is
`Feed a -> sOut -> Maybe sIn`, requiring `Constriction sOut b` and
`Constriction sIn a`. A `Feed a` state is `{ value : a, pinned : Maybe a }`: the latest candidate
and an optional whole-value pin ([feeds and re-solving](unravel-design.md#2-feeds-and-rules)).
Multiple arguments use a tuple of input states and a tuple of returned constraints,
in argument order, with no arity cap; a single argument remains unwrapped.
The engine resolves requests into source edits and validates forward execution.
Authors supply candidate-selection rules. Different constraint types compose through explicit typed adapters, without
general conversion-path search.

Multiple attached rules select by output-constraint type: exact matches first,
then applicable polymorphic matches, with annotation definition order breaking
ties in each group. Order is top-to-bottom, left-to-right within a block. Adapters are explicit; there is no implicit conversion search. No type-distance
ranking, caller-type refinement or implicit retry after rule failure. The chosen
rule determines its upstream constraint types. See [rule selection](unravel-design.md#2-feeds-and-rules).
Unary registration/dispatch is implemented at the opt-in edit boundary; adapters,
multi-argument registration and source-attached higher-order companions are
implemented by the workbench; see [current limits](knot_v0.md).

### 16.2 Default unravelers

Builtins should provide unravel rules, but no default constraint representation
is imposed on authors. A rule's choice of inputs/alternatives is its own policy.
For v0 materialization, a type supplies a default constructor and a default
unravel that edits its starting value to satisfy the request; validate the result
before inserting a fresh construction expression into source.

### 16.3 Unravel on higher-order functions

Forward the callback's typed unravel companion into the higher-order unravel;
its constraint types are chosen by its author, not derived from `a -> b`.
`map` preserves those types, including range constraints. Captures and supplied
arguments stay fixed. Dynamically chosen callbacks need compatible reverse
contracts or declared adapters; otherwise reverse editing is unsupported.

For growth, create a fresh input draft from a materialization seed, call the
callback's unravel on that same seed, and validate before applying a source patch.
Do not rely on replaying only the existing trace. See the
[current edit-and-growth example](unravel-design.md#example-edit-and-grow-mapped-inputs).
Generated companions and validated source insertion are implemented. Bounded
pushforward/re-solving is described in [feeds and re-solving](unravel-design.md#2-feeds-and-rules).

### 16.4 Conflict resolution

One candidate must satisfy every request and pin on a shared input; do not average
conflicts. Recompute current values separately from pins; only fully fixed inputs
imply a fixed output. Rules can re-solve a free argument when another is pinned.
V0 tries supplied request order, then its reverse, with a shared budget and at most
two attempts. Apply only fully validated edits against the original revision;
ordinary failure does not prove unsatisfiability. General partial-pin propagation
is deferred. See the [executable example](corpus/run/unravel-pushforward.knot).

### 16.5 Annotation examples

```knot
@{ unravel = myDomainUnraveler }
output = domainSpecificOp input
```

This illustrates attachment only. Tangle also needs an “unraveller” output node
connecting the value to edit with the application's displayed output object.

### 16.6 `Sensitivity` as a Recursive Type Function

`Sensitivity :: Type -> Type` is an optional helper, not the unravel contract.
Authors may instead use exact values, optional targets, alternatives, structural
list constraints or domain-specific types. Helper expansion and instance coherence
remain open. The current opaque `Sensitivity` stub does not implement expansion.

## 17. Typed Holes

### 17.1 Expression holes

`_` is an expression placeholder: checking reports its inferred type and rejects
the program. Named expression holes (`_name`) are unsupported.

### 17.2 Pattern match and Binding holes (`let _ = ...`)

`_` / `_name` in patterns discard values and bind no usable name.
`let _ = expression in body` is valid and does not force the discarded expression;
it is still checked. `_name` is only a mnemonic.

### 17.3 Annotation compatibility

Holes cannot carry annotations. Annotate the enclosing expression or binding.

## 18. Diagnostics: Errors & Warnings

**Errors:** syntax/name/type/kind errors, ambiguous constraints, missing or
conflicting instances, invalid instance heads/methods/superclasses, and expression
holes. **Warnings:** incomplete or unreachable case arms.

Runtime failures include explicit errors, failed matches, invalid primitive
operations and recursive black holes. Twine resource exhaustion/cancellation is
reported separately; see the [runtime guide](compiler/twine/README.md).

## 19. Open Questions

- Final unravel input/target/failure types, higher-order behavior, shape edits,
  conflict solving, and UI attachment (§16).
- Annotation key typing, evaluation timing and extensibility (§15).
- Structured logging and richer application IO; basic console IO already works.
- Whether to add range syntax or user constructor operators. Today use
  `enumFromTo`, and user `:`-leading operators remain functions.

## 20. Planned for v2

- **User-defined interfaces:** extend the existing constraint/dictionary system.
- **Unit-aware numeric types:** nominal lengths, speeds, etc., with explicit scaling.
