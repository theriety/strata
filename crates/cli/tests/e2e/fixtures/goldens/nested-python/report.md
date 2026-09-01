# Strata report

Snapshot `908dd56719a4dd47cdb57e3c77b0a96aadd6b26edac973edda49371019b2da15`.

- 12 symbols, 15 edges, 6 files

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
relocation pin-test-files true · pin-test-symbols true · file patterns 0 · symbol patterns 0
mirroring enabled true · builtins true · rules 0
```

Current score `0.3844`.

- cut `0.3170`, imbalance `0.2790`, naming `-0.0115`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

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
relocation pin-test-files true · pin-test-symbols true · file patterns 0 · symbol patterns 0
mirroring enabled true · builtins true · rules 0
```

Current score `0.5844`.

- cut `0.3170`, imbalance `0.2790`, naming `-0.0115`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.2310`, score `0.3535`)

- cut `0.2091`, imbalance `0.1444`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — pulled by rectangle.py (w 2.0)
  1. nested-python/app.py [nested-python → nested-python/geometry]
```

