# Local Knot modules

The CLI and `knot-workbench::modules::ModuleGraph` load and check a local import
graph. Each module keeps its own typed AST, nominal identities, revision and source
locations. Dependency bodies are linked into one lazy executable program.

## Files and commands

`import Geometry.Mesh` resolves to `Geometry/Mesh.knot` beneath a module root.
By default the root is the entry file's directory. Use repeatable
`--module-root DIR` options for explicit roots; when present, these replace the
default. Multiple distinct files matching an import are an error, regardless of
root order. Canonical paths deduplicate symlinks/repeated roots. The dependency's
module header must match the requested name. The entry file is explicitly chosen
and may have any filename.

```sh
knot check app/Main.knot --module-root src
knot run app/Main.knot --module-root src
knot eval app/Main.knot result --module-root src
knot inspect app/Main.knot result 'show value' --module-root src
```

All five commands (`check`, `core`, `run`, `eval`, `inspect`) use the loader.
For the repository drawing:

```sh
./knotty-drawings/run.sh example-drawing.knot
```

There is no concatenation step. `Types.knot` and `KnottyDrawings.knot` use their
module names as filenames. The examples explicitly import unqualified names with
`exposing (..)`: an ordinary `import X` only enables qualified references.

## Interfaces and instances

Module export lists expose locally defined values and types. `T` hides its
constructors; `T(..)` exposes them. Imported aliases expand to their checked
bodies, preserving any private nominal types inside. Private helpers stay
linkable but cannot be named by consumers. Invalid or duplicate exposing entries
are rejected even when unused. Re-exporting imported names is not supported yet.

Checked interfaces carry generalized schemes, constructor arities, fully resolved
kinds, expanded aliases, operator fixities and instance contexts. Every consumer
gets fresh type variables; raw arena indices never identify a type across modules.
The interface currently owns an in-memory type arena; it is not a disk format.

Instances are visible through transitive imports, including imports without an
exposing clause. Each dependency contributes its own instances once, including in
diamonds. Conflicting instances are errors across the complete reachable graph;
loading order does not select an implementation. A sibling module's instances do
not affect checking unless imported. Builtin and registered host module names are
reserved. Registered native types and host calls work through user modules.

The checked linker rejects duplicate globals, unresolved globals and inconsistent
host signatures. Runtime demand traces retain the original dependency file and
source span. Effects still require explicitly running an IO action.

## Rust embedding

1. Call `ModuleGraph::load(entry, roots, host_declarations)`, or
   `load_with_source` to replace the entry text without writing a temporary file.
   `modules` is ordered dependencies first, with the entry last. Retain each
   module's source and revision for diagnostics/Tangle.
2. Optionally call `graph.prepare_query(module_name, exact_span, expression)`.
   Replace that module's typed program with the returned program and use its
   diagnostic source for that revision. Queries check against the same imported
   interfaces and still cannot change the original bindings' types/constraints.
3. Obtain bundled library core via `Libraries::load`, then pass it and the graph's
   typed programs to `knot_core::link::link`. Pass the same host declarations used
   during checking.
4. Create a Twine session (`Session::with_hosts` for native extensions) and demand
   the desired roots. Prepared inspector queries remain lazy and bounded.

A prepared query still requires a new session; this is not live query attachment.

## Boundaries and next work

Cyclic module imports, package/network resolution, re-exports, incremental
compilation and disk serialization of user interfaces are deferred. Recursive
bindings within a module work. Module files are all loaded/checked before execution;
language laziness does not imply lazy filesystem loading.

User modules are rebuilt on every load. Only bundled-library core has a compilation
cache. Revision identities and opt-in dependency tracking are
implemented separately. Next are closed-result/native-snapshot reuse across edits,
then lazy structures and closures. Runtime reuse does not require a module
compilation cache.
