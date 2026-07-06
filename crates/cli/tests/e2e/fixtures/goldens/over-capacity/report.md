# Strata report

Snapshot `16329a520c6b6c1c80fed2150091544b609f46d4cf39f2094ccab7a8a66bf60d`.

- 375 symbols, 0 edges, 3 files

## Current layout

Score `-0.0611`.

- cut `-0.0000`, imbalance `0.0095`, naming `-0.0000`, path `-0.0706`, anchor `0.0000`

### Violations

- **Capacity** (Violation) at over-capacity, pkg, nested, deep, huge_nested.py: file `pkg/nested/deep/huge_nested.py` holds 300 against a cap of 250
- **Capacity** (Violation) at over-capacity, workspace, huge.py: file `huge.py` holds 300 against a cap of 250
- **Capacity** (Borderline) at over-capacity, workspace, borderline.py: file `borderline.py` holds 250 against a cap of 250

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout violates capacity caps; best candidate resolves 0 of 2 capacity finding(s)._

_2 file-level breach(es) exceed the file cap; only conditional splits can fix them._

### Candidate 1 (improvement `-0.2785`, score `0.2174`)

- cut `-0.0000`, imbalance `0.0007`, naming `-0.0500`, path `-0.0000`, anchor `0.2667`

**Moves**

- Move `pkg/nested/deep/huge_nested.py`: over-capacity/pkg/nested/deep -> over-capacity/workspace (regrouped by clustering)

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout violates capacity caps; best candidate resolves 0 of 2 capacity finding(s)._

_2 file-level breach(es) exceed the file cap; only conditional splits can fix them._

### Candidate 1 (improvement `+0.0588`, score `-0.0493`)

- cut `-0.0000`, imbalance `0.0007`, naming `-0.0500`, path `-0.0000`, anchor `0.0000`

**Moves**

- Move `pkg/nested/deep/huge_nested.py`: over-capacity/pkg/nested/deep -> over-capacity/workspace (regrouped by clustering)

