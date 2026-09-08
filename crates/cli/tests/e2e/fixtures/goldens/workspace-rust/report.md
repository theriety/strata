# Strata report — workspace-rust

3 files · 14 symbols · 13 edges
Snapshot `a796634250f1de8a466fe7acac2cb579eb4f6dce97b7ce5260598fcf732dca0b`

## Structural findings

```text
Shared findings (4)
  - visibility [violation] at `crates/app/src/lib.rs`, `ReMeasure`: `ReMeasure` in 
    `crates/app/src/lib.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/app/src/lib.rs`, `combined_total`: `combined_total` in 
    `crates/app/src/lib.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/app/src/lib.rs`, `describe`: `describe` in 
    `crates/app/src/lib.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/core/src/lib.rs`, `Report`: `Report` in 
    `crates/core/src/lib.rs` is exported at Package but needed only at File

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
Baseline score: 0.6810
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 0 candidate(s)
Baseline score: 0.8810
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
