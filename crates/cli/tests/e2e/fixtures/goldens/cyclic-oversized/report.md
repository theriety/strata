# Strata report — cyclic-oversized

3 files · 3 symbols · 3 edges
Snapshot `55784e8d4ae083e616cd25e5fb9c5c3a94ea39ee8dbb7bc6dfe7ad6e1da7312c`

## Structural findings

```text
Shared findings (1)
  - cycle [violation] `alpha.ts`/`beta.ts`/`gamma.ts` — 3-symbol cycle; symbols form one placement 
    unit and must remain in one file unless a suggested dependency edge is broken; break 
    `stepGamma` -> `stepAlpha` (w=1.0, exact)

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
