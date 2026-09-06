# Strata report

Snapshot `908dd56719a4dd47cdb57e3c77b0a96aadd6b26edac973edda49371019b2da15`.

- 12 symbols, 15 edges, 6 files

## Relocation advice

```text
 Evidence: owner means unique ownership; role means role affinity; source and destination mean
 cohesion at each side; producer means producer evidence; reach means architectural reach.
 Weighted is the normalized score across all six signals; structural excludes role affinity; margin
 is the selected destination's lead over the best alternative.
 The values after weighted and structural, and the margin threshold, are configured minimums.
 Profiles share analysis-start evidence but apply their own weights and thresholds.
 Recommended (0):
 Review candidate (3):
   - file `nested-python/app.py` → `nested-python/geometry` · supporting [greenfield] · qualified
   [] · absent [anchored] · conflicts []
     greenfield: owner 0.00 · role 0.00 · source 1.00 · destination 1.00 · producer 0.00 · reach
     0.00 · margin 0.48; weighted 0.50/0.60 · structural 0.56/0.50 · margin threshold 0.15 ·
     qualified false · best alternative `nested-python`
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, no strict majority of executed profiles provides qualifying support for
     this destination
   - `describe` from `geometry/rectangle.py` → `app.py` · supporting [greenfield] · qualified [] ·
   absent [anchored] · conflicts []
     greenfield: owner 0.00 · role 0.00 · source 1.00 · destination 0.38 · producer 0.00 · reach
     0.00 · margin 0.07; weighted 0.35/0.60 · structural 0.38/0.50 · margin threshold 0.15 ·
     qualified false · best alternative `geometry/shape.py`
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination
   - type `Shape` from `geometry/shape.py` → `geometry/rectangle.py` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     greenfield: owner 0.00 · role 0.00 · source 1.00 · destination 0.78 · producer 0.00 · reach
     0.00 · margin 0.17; weighted 0.45/0.60 · structural 0.50/0.50 · margin threshold 0.15 ·
     qualified false · best alternative `app.py`
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, no strict majority of executed
     profiles provides qualifying support for this destination

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
qualification evidence 0.6 · structural 0.5 · ambiguity-margin 0.15
owner 0.1 · role 0.1 · source 0.25 · destination 0.25
producer 0.1 · architectural-reach 0.2
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
 - move `describe` from `geometry/rectangle.py` to `app.py` (delta -0.0122, 2 import(s) to re-point)
 - move type `Shape` from `geometry/shape.py` to `geometry/rectangle.py` (delta -0.0017, 2 import(s) to re-point)
```

