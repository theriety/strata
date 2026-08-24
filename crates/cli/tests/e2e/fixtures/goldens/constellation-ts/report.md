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

### Candidate 1 (improvement `+0.0256`, score `0.1412`)

- cut `0.2135`, imbalance `0.0425`, naming `-0.0016`, path `-0.1846`, anchor `0.0714`

**Moves**

```
move — pulled by point.ts (w 1.0)
  1. src/io/parser.ts [constellation-ts/io → constellation-ts/model]
move — pulled by canvas.ts (w 1.0)
  2. src/core/epsilon.ts [constellation-ts/core → constellation-ts/render]
```

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.1504`, score `0.2164`)

- cut `0.1797`, imbalance `0.0398`, naming `-0.0031`, path `-0.0000`, anchor `0.0000`

**Moves**

```
split — pulled by beta.ts (w 1.0)
  1. src/util/clamp.ts [constellation-ts/util → constellation-ts/core]
merge — pulled by matrix.ts (w 1.0)
  2. src/io/parser.ts [constellation-ts/io → constellation-ts/model]
  3. src/util/hash.ts [constellation-ts/util → constellation-ts/model]
move — pulled by canvas.ts (w 1.0)
  4. src/core/epsilon.ts [constellation-ts/core → constellation-ts/render]
split — follows canvas.ts
  5. src/util/canvas.spec.ts [constellation-ts/util → constellation-ts/render]
```

