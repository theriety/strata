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

### Candidate 1 (improvement `+0.1628`, score `0.2040`)

- cut `0.1641`, imbalance `0.0435`, naming `-0.0036`, path `-0.0000`, anchor `0.0000`

**Moves**

```
merge — pulled by gamma.ts (w 2.0)
  1. src/model/matrix.ts [constellation-ts/model → constellation-ts/core]
  2. src/model/mesh.ts [constellation-ts/model → constellation-ts/core]
  3. src/util/clamp.ts [constellation-ts/util → constellation-ts/core]
  4. src/util/hash.ts [constellation-ts/util → constellation-ts/core]
split — pulled by encoder.ts (w 1.0)
  5. src/util/buffer.ts [constellation-ts/util → constellation-ts/io]
merge — pulled by point.ts (w 2.0)
  6. src/core/alpha.ts [constellation-ts/core → constellation-ts/model]
  7. src/io/parser.ts [constellation-ts/io → constellation-ts/model]
  8. src/util/lerp.ts [constellation-ts/util → constellation-ts/model]
merge — pulled by logger.ts (w 2.0)
  9. src/core/epsilon.ts [constellation-ts/core → constellation-ts/util]
  10. src/io/writer.ts [constellation-ts/io → constellation-ts/util]
  11. src/render/canvas.ts [constellation-ts/render → constellation-ts/util]
  12. src/render/shader.ts [constellation-ts/render → constellation-ts/util]
```

