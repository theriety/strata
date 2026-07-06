# Strata report

Snapshot `7ade14d1273c13e52915520cceca5632b33d900bd8c849da1e95662246682e40`.

- 28 symbols, 48 edges, 28 files

## Current layout

Score `251.8387`.

- cut `252.0000`, imbalance `0.0406`, naming `-0.0019`, path `-0.2000`, anchor `0.0000`

### Violations

None.

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+141.0413`, score `110.7974`)

- cut `110.0000`, imbalance `0.0161`, naming `-0.0017`, path `-0.0385`, anchor `0.8214`

**Moves**

- Merge 7 file(s): constellation-ts/src/io, constellation-ts/src/model, constellation-ts/src/util -> constellation-ts/src/core (pulled by beta.ts (w 2.0))
  - `src/io/parser.ts`
  - `src/model/point.ts`
  - `src/model/shape.ts`
  - `src/model/vector.ts`
  - `src/util/buffer.ts`
  - `src/util/clamp.ts`
  - `src/util/lerp.ts`
- Split `src/model/shape.spec.ts`: constellation-ts/src/model -> constellation-ts/src/core (follows point.ts)
- Merge 14 file(s): constellation-ts/src/core, constellation-ts/src/io, constellation-ts/src/model, constellation-ts/src/render, constellation-ts/src/util -> constellation-ts/src/core/render (pulled by canvas.spec.ts (w 1.0))
  - `src/core/epsilon.ts`
  - `src/io/encoder.ts`
  - `src/io/reader.ts`
  - `src/io/stream.ts`
  - `src/io/writer.ts`
  - `src/model/matrix.ts`
  - `src/model/mesh.ts`
  - `src/render/canvas.ts`
  - `src/render/raster.ts`
  - `src/render/shader.ts`
  - `src/render/texture.ts`
  - `src/render/viewport.ts`
  - `src/util/hash.ts`
  - `src/util/logger.ts`
- Split `src/util/canvas.spec.ts`: constellation-ts/src/util -> constellation-ts/src/core/render (follows canvas.ts)

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+142.0243`, score `110.0144`)

- cut `110.0000`, imbalance `0.0161`, naming `-0.0017`, path `-0.0000`, anchor `0.0000`

**Moves**

- Merge 7 file(s): constellation-ts/src/io, constellation-ts/src/model, constellation-ts/src/util -> constellation-ts/src/core (pulled by beta.ts (w 2.0))
  - `src/io/parser.ts`
  - `src/model/point.ts`
  - `src/model/shape.ts`
  - `src/model/vector.ts`
  - `src/util/buffer.ts`
  - `src/util/clamp.ts`
  - `src/util/lerp.ts`
- Split `src/model/shape.spec.ts`: constellation-ts/src/model -> constellation-ts/src/core (follows point.ts)
- Merge 14 file(s): constellation-ts/src/core, constellation-ts/src/io, constellation-ts/src/model, constellation-ts/src/render, constellation-ts/src/util -> constellation-ts/src/core/render (pulled by canvas.spec.ts (w 1.0))
  - `src/core/epsilon.ts`
  - `src/io/encoder.ts`
  - `src/io/reader.ts`
  - `src/io/stream.ts`
  - `src/io/writer.ts`
  - `src/model/matrix.ts`
  - `src/model/mesh.ts`
  - `src/render/canvas.ts`
  - `src/render/raster.ts`
  - `src/render/shader.ts`
  - `src/render/texture.ts`
  - `src/render/viewport.ts`
  - `src/util/hash.ts`
  - `src/util/logger.ts`
- Split `src/util/canvas.spec.ts`: constellation-ts/src/util -> constellation-ts/src/core/render (follows canvas.ts)

