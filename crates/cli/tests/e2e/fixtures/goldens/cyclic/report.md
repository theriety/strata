# Strata report — cyclic

2 files · 2 symbols · 2 edges
Snapshot `f98d0366a2bb968bb509b5e4f5d1657f2ddfed80b1b1bbc5f8cb211a376d6839`

## Structural findings

```text
Shared findings (1)
  - cycle [violation] `alpha.py`/`beta.py` — 2-symbol cycle; symbols form one placement unit and 
    must remain in one file unless a suggested dependency edge is broken; break `beta_step` -> 
    `alpha_step` (w=1.0, exact)

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
Baseline score: -0.0750
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 0 candidate(s)
Baseline score: 0.1250
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
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
