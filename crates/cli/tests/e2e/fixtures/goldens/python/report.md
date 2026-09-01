# Strata report

Snapshot `9d2353ef7b54118fc475c73afad47f718dc9e9560acd449a3768b80ad8f4811c`.

- 4 symbols, 4 edges, 2 files

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
qualification evidence 0.6 · structural 0.5 · ambiguity-margin 0.15
owner 0.1 · role 0.1 · source 0.25 · destination 0.25
producer 0.1 · architectural-reach 0.2
```

Current score `-0.0901`.

- cut `0.0988`, imbalance `0.0111`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

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
qualification evidence 0.6 · structural 0.5 · ambiguity-margin 0.15
owner 0.1 · role 0.1 · source 0.25 · destination 0.25
producer 0.1 · architectural-reach 0.2
```

Current score `0.1099`.

- cut `0.0988`, imbalance `0.0111`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

