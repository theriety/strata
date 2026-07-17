# Strata report

Snapshot `12763e5c2516c0f237c53ced17b567cf4a35f95fec2ccc63d3c09bf8866a1d81`.

- 28 symbols, 48 edges, 28 files

## Current layout

Score `251.8387`.

- cut `252.0000`, imbalance `0.0406`, naming `-0.0019`, path `-0.2000`, anchor `0.0000`

### Violations

None.

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+133.6845`, score `118.1542`)

- cut `118.0000`, imbalance `0.0610`, naming `-0.0029`, path `-0.1538`, anchor `0.2500`

**Moves**

```
split — pulled by beta.ts (w 1.0)
  1. src/util/clamp.ts [constellation-ts/util → constellation-ts/core]
split — pulled by reader.ts (w 1.0)
  2. src/util/logger.ts [constellation-ts/util → constellation-ts/io]
merge — pulled by point.ts (w 2.0)
  3. src/io/parser.ts [constellation-ts/io → constellation-ts/model]
  4. src/util/hash.ts [constellation-ts/util → constellation-ts/model]
  5. src/util/lerp.ts [constellation-ts/util → constellation-ts/model]
move — pulled by canvas.ts (w 1.0)
  6. src/core/epsilon.ts [constellation-ts/core → constellation-ts/render]
split — follows canvas.ts
  7. src/util/canvas.spec.ts [constellation-ts/util → constellation-ts/render]
```

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+133.9806`, score `118.0580`)

- cut `118.0000`, imbalance `0.0610`, naming `-0.0029`, path `-0.0000`, anchor `0.0000`

**Moves**

```
split — pulled by beta.ts (w 1.0)
  1. src/util/clamp.ts [constellation-ts/util → constellation-ts/core]
split — pulled by reader.ts (w 1.0)
  2. src/util/logger.ts [constellation-ts/util → constellation-ts/io]
merge — pulled by point.ts (w 2.0)
  3. src/io/parser.ts [constellation-ts/io → constellation-ts/model]
  4. src/util/hash.ts [constellation-ts/util → constellation-ts/model]
  5. src/util/lerp.ts [constellation-ts/util → constellation-ts/model]
move — pulled by canvas.ts (w 1.0)
  6. src/core/epsilon.ts [constellation-ts/core → constellation-ts/render]
split — follows canvas.ts
  7. src/util/canvas.spec.ts [constellation-ts/util → constellation-ts/render]
```

