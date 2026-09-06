# Strata report

Snapshot `d6148cfe4411734862d94e4add3a70b0a8ac70d0524143a57e21f0f9660b1141`.

- 12 symbols, 13 edges, 6 files

## Relocation advice

```text
 Evidence: owner means unique ownership; role means role affinity; source and destination mean
 cohesion at each side; producer means producer evidence; reach means architectural reach.
 Weighted is the normalized score across all six signals; structural excludes role affinity; margin
 is the selected destination's lead over the best alternative.
 The values after weighted and structural, and the margin threshold, are configured minimums.
 Profiles share analysis-start evidence but apply their own weights and thresholds.
 Recommended (0):
 Review candidate (1):
   - file `nested-ts/src/app.ts` → `nested-ts/src/geometry` · supporting [greenfield] · qualified
   [] · absent [anchored] · conflicts []
     greenfield: owner 0.00 · role 0.00 · source 1.00 · destination 1.00 · producer 0.00 · reach
     0.00 · margin 0.43; weighted 0.50/0.60 · structural 0.56/0.50 · margin threshold 0.15 ·
     qualified false · best alternative `nested-ts/src`
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, no strict majority of executed profiles provides qualifying support for
     this destination

```

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

Current score `0.2553`.

- cut `0.3363`, imbalance `0.1190`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

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

Current score `0.4553`.

- cut `0.3363`, imbalance `0.1190`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+0.0899`, score `0.3654`)

- cut `0.2306`, imbalance `0.1347`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — pulled by rectangle.ts (w 2.0)
  1. nested-ts/src/app.ts [nested-ts/src → nested-ts/src/geometry]
```

