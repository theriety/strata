# Strata report

Snapshot `8795580f9da7539c0f81cfff9a5a94ff00821c1c6231cd0d0a6934cff6e2e7ac`.

- 14 symbols, 13 edges, 3 files

## Shared findings

### Violations

- **Visibility** (Violation) at ReMeasure: `ReMeasure` is exported at Package but needed only at File
- **Visibility** (Violation) at Report: `Report` is exported at Package but needed only at File
- **Visibility** (Violation) at combined_total: `combined_total` is exported at Package but needed only at File
- **Visibility** (Violation) at describe: `describe` is exported at Package but needed only at File

## Anchored parameter profile

### Effective parameters

```text
anchored parameter profile (effective):
search    candidates 3 · seed 42
capacity  file 250 · folder 20 · domain 16 · package 15 · package-group 12
objective imbalance 0.1 · naming 0.3 · path 0.2 · anchor 1.0 · capacity 4.0
dependency-only 0.05
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.6810`.

- cut `0.8756`, imbalance `0.0054`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`

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
dependency-only 0.05
weights   value-import 1.0 · inheritance 1.5 · call 1.0 · type-reference 0.3 · re-export 0.0
same-file-symbol 1.0 · same-file-type 3.0
solver    ilp-threshold 300 · timeout-seconds 60
diversity seeds-per-candidate 10 · score-tolerance 0.05 · min-distance 0.05
tests     helper-cap 250 · patterns 0 · builtins true
```

Current score `0.8810`.

- cut `0.8756`, imbalance `0.0054`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

