# Strata report

Snapshot `0ae026419631a59131d81f1e90ba4c679e96a74d71394ac168f3a15263901bc8`.

- 12 symbols, 13 edges, 6 files

## Shared findings

### Violations

None.

## Anchored parameter profile

### Effective parameters

```text
anchored parameter profile (effective):
search    candidates 3 · seed 42
capacity  file 250 · folder 20 · domain 16 · package 15 · package-group 12
objective imbalance 0.1 · naming 0.3 · path 0.2 · anchor 1.0 · capacity 4.0
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.2553`.

- cut `0.3363`, imbalance `0.1190`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

### Candidate 1 (improvement `+0.0000`, score `0.2553`)

- cut `0.3363`, imbalance `0.1190`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

No moves versus the current layout.

## Greenfield parameter profile

### Effective parameters

```text
greenfield parameter profile (effective):
search    candidates 3 · seed 42
capacity  file 250 · folder 20 · domain 16 · package 15 · package-group 12
objective imbalance 0.1 · naming 0.3 · path 0.0 · anchor 0.0 · capacity 4.0
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.4553`.

- cut `0.3363`, imbalance `0.1190`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.0712`, score `0.3840`)

- cut `0.2306`, imbalance `0.1684`, naming `-0.0150`, path `-0.0000`, anchor `0.0000`

**Moves**

```
move — pulled by rectangle.ts (w 2.0)
  1. nested-ts/src/app.ts [nested-ts/src → nested-ts/src/geometry]
move — follows app.ts
  2. nested-ts/src/__tests__/app.spec.ts [nested-ts/src/__tests__ → nested-ts/src/geometry]
```

