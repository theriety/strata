# Strata report

Snapshot `12763e5c2516c0f237c53ced17b567cf4a35f95fec2ccc63d3c09bf8866a1d81`.

- 28 symbols, 48 edges, 28 files

## Current layout

Score `0.1668`.

- cut `0.3281`, imbalance `0.0406`, naming `-0.0019`, path `-0.2000`, anchor `0.0000`

### Violations

None.

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

### Candidate 1 (improvement `+0.0000`, score `0.1668`)

- cut `0.3281`, imbalance `0.0406`, naming `-0.0019`, path `-0.2000`, anchor `0.0000`

No moves versus the current layout.

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.1504`, score `0.2164`)

- cut `0.1797`, imbalance `0.0398`, naming `-0.0031`, path `-0.0000`, anchor `0.0000`

**Moves**

```
split — pulled by beta.ts (w 1.0)
  1. constellation-ts/src/util/clamp.ts [constellation-ts/src/util → constellation-ts/src/core]
merge — pulled by matrix.ts (w 1.0)
  2. constellation-ts/src/io/parser.ts [constellation-ts/src/io → constellation-ts/src/model]
  3. constellation-ts/src/util/hash.ts [constellation-ts/src/util → constellation-ts/src/model]
move — pulled by canvas.ts (w 1.0)
  4. constellation-ts/src/core/epsilon.ts [constellation-ts/src/core → constellation-ts/src/render]
split — follows canvas.ts
  5. constellation-ts/src/util/canvas.spec.ts [constellation-ts/src/util → constellation-ts/src/render]
```

