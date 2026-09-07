# Strata report — over-capacity

3 files · 375 symbols · 0 edges
Snapshot `2932fab8459e42d588351d1accd9d3c7e0c6dce7ae161cf0a465f43d90fe4684`

## Structural findings

```text
Shared findings (2)
  - capacity [violation] file `huge.py` holds 300 against a cap of 250
  - capacity [violation] file `pkg/nested/deep/huge_nested.py` holds 300 against a cap of 250

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.

```

## Candidate layouts

```text
Lower scores are better. Compare scores only within one parameter profile of one project. A 
candidate is a proposed layout; inclusion does not make a move Recommended. Partial plans are not 
rescored.

anchored — 0 candidate(s)
Baseline score: 1.4095
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).
No candidates were produced.

greenfield — 0 candidate(s)
Baseline score: 1.6095
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).
No candidates were produced.
```

## Advice

```text
Supporting profiles selected the destination; qualified profiles also passed the evidence 
thresholds; absent profiles did not select the move; conflicts name alternative destinations. 
Advice consolidates the best candidate from each executed profile.
 Recommended (0):
 Review candidate (0):

```
