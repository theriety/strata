# Strata report

Snapshot `7ebf6d1c547dcf9580fbaf51fe0fb020678583078c443f5f6531987a5dfe21be`.

- 24 symbols, 22 edges, 24 files

## Shared findings

### Violations

- **Capacity** (Violation) at relief-nest-py, hub: folder `relief-nest-py/hub` holds 24 against a cap of 20

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

Current score `0.6728`.

- cut `0.1250`, imbalance `0.0000`, naming `-0.0522`, path `-0.2000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout violates capacity caps; best candidate resolves 1 of 1 capacity finding(s)._

### Candidate 1 (improvement `+0.2478`, score `0.4250`)

- cut `0.1250`, imbalance `0.0000`, naming `-0.1000`, path `-0.1000`, anchor `0.5000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — relieves over-cap folder relief-nest-py/hub (24/20 entries)
  1. relief-nest-py/hub/ingest_00.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  2. relief-nest-py/hub/ingest_01.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  3. relief-nest-py/hub/ingest_02.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  4. relief-nest-py/hub/ingest_03.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  5. relief-nest-py/hub/ingest_04.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  6. relief-nest-py/hub/ingest_05.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  7. relief-nest-py/hub/ingest_06.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  8. relief-nest-py/hub/ingest_07.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  9. relief-nest-py/hub/ingest_08.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  10. relief-nest-py/hub/ingest_09.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  11. relief-nest-py/hub/ingest_10.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  12. relief-nest-py/hub/ingest_11.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
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
relocation pin-test-files true · pin-test-symbols true · file patterns 0 · symbol patterns 0
mirroring enabled true · builtins true · rules 0
```

Current score `0.8728`.

- cut `0.1250`, imbalance `0.0000`, naming `-0.0522`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

### Profile-specific findings

### Violations

None.

### Candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout violates capacity caps; best candidate resolves 1 of 1 capacity finding(s)._

### Candidate 1 (improvement `+0.8478`, score `0.0250`)

- cut `0.1250`, imbalance `0.0000`, naming `-0.1000`, path `-0.0000`, anchor `0.0000`, dependency-only `0.0000`, companion-separation `0.0000`

**Moves**

```
move — relieves over-cap folder relief-nest-py/hub (24/20 entries)
  1. relief-nest-py/hub/ingest_00.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  2. relief-nest-py/hub/ingest_01.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  3. relief-nest-py/hub/ingest_02.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  4. relief-nest-py/hub/ingest_03.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  5. relief-nest-py/hub/ingest_04.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  6. relief-nest-py/hub/ingest_05.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  7. relief-nest-py/hub/ingest_06.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  8. relief-nest-py/hub/ingest_07.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  9. relief-nest-py/hub/ingest_08.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  10. relief-nest-py/hub/ingest_09.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  11. relief-nest-py/hub/ingest_10.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
  12. relief-nest-py/hub/ingest_11.py [relief-nest-py/hub → relief-nest-py/hub/ingest]
```

