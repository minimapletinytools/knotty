# Creation boundaries

`Constriction.materialize` supplies candidate values at engine-supported creation
and replacement boundaries. Lists materialize their missing suffix; products need
a candidate for every new field. Unspecified fields without defaults fail.

The engine reconstructs source from the value. Source templates and `@materialize`
registrations are removed. Preserving factory-call expressions and wrapping existing
source are deferred. See factory policies for ordinary
Knot helpers that construct candidates.
