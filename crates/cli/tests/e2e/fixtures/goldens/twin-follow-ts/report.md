# Strata report

Snapshot `52e6f14a8dc5624b5a8c5f07220f4e35022fab3ffdb40b804590074420b43892`.

- 2 symbols, 1 edges, 2 files

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
dependency-only 0.05 · companion-separation 0.05
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.4000`.

- cut `0.5000`, imbalance `0.1000`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.0250`, score `0.3750`)

- cut `0.1250`, imbalance `0.1000`, naming `-0.1500`, path `-0.2000`, anchor `0.5000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — follows openai.ts
  1. twin-follow-ts/src/specs/openai.spec.ts [twin-follow-ts/src/specs → twin-follow-ts/src/api]
```

## Greenfield parameter profile

### Effective parameters

```text
greenfield parameter profile (effective):
search    candidates 3 · seed 42
capacity  file 250 · folder 20 · domain 16 · package 15 · package-group 12
objective imbalance 0.1 · naming 0.3 · path 0.0 · anchor 0.0 · capacity 4.0
dependency-only 0.05 · companion-separation 0.05
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.6000`.

- cut `0.5000`, imbalance `0.1000`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.5250`, score `0.0750`)

- cut `0.1250`, imbalance `0.1000`, naming `-0.1500`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — follows openai.ts
  1. twin-follow-ts/src/specs/openai.spec.ts [twin-follow-ts/src/specs → twin-follow-ts/src/api]
```

