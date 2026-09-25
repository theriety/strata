# Strata report — rust-cfg-test

1 files · 4 symbols · 2 edges
Snapshot `72472a4d36d1b5f02fb5368a6a5c3733a3b862ca59b20f489506f8bee0c57544`

## Structural findings

```text
Shared findings (2)
  - visibility [violation] at `src/lib.rs`, `double`: `double` in `src/lib.rs` is exported at 
    Package but needed only at File
  - visibility [violation] at `src/lib.rs`, `halve`: `halve` in `src/lib.rs` is exported at 
    Package but needed only at File

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
Baseline score: -0.1375
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 0 candidate(s)
Baseline score: 0.0625
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
