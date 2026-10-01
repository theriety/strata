# Strata report — workspace-move-rust

8 files · 11 symbols · 17 edges
Snapshot `3027913ba8b0852017ba2a3f16bcc60569d3dff3845df8291300559271a3995c`

## Structural findings

```text
Shared findings (6)
  - visibility [violation] at `crates/core/src/alpha/parse.rs`, `parse`: `parse` in 
    `crates/core/src/alpha/parse.rs` is exported at Package but needed only at Folder
  - visibility [violation] at `crates/core/src/alpha/parse.rs`, `parse_max`: `parse_max` in 
    `crates/core/src/alpha/parse.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/core/src/alpha/score.rs`, `score`: `score` in 
    `crates/core/src/alpha/score.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/core/src/alpha/score.rs`, `score_pair`: `score_pair` in 
    `crates/core/src/alpha/score.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/core/src/beta/report.rs`, `render`: `render` in 
    `crates/core/src/beta/report.rs` is exported at Package but needed only at File
  - visibility [violation] at `crates/core/src/beta/report.rs`, `render_all`: `render_all` in 
    `crates/core/src/beta/report.rs` is exported at Package but needed only at File

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
Baseline score: 0.5846
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.7846
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.7846 → 0.4289; improvement +0.3557
  Proposed moves
    - Move file `crates/core/src/beta/scale.rs` → `crates/core/src/alpha/scale.rs`
      Why: pulled toward `crates/core/src/alpha/parse.rs` — weight 5.0.
    - Move symbol `unit` from `crates/core/src/beta/scale.rs` → `crates/core/src/alpha/parse.rs`
  Folder impacts
    `crates/core/src/alpha`: 0 file(s) leaving · 1 entering
    `crates/core/src/beta`: 1 file(s) leaving · 0 entering
  Before
    workspace-move-rust/
    └── crates/
        └── core/
            └── src/
                ├── alpha/
                │   └── parse.rs *
                └── beta/
                    └── scale.rs *
                        └── symbol `unit` [to crates/core/src/alpha/parse.rs]
  After
    workspace-move-rust/
    └── crates/
        └── core/
            └── src/
                └── alpha/
                    ├── parse.rs *
                    │   └── symbol `unit` [from crates/core/src/beta/scale.rs]
                    └── scale.rs *
  * File moved or its symbol contents changed.
  Unchanged branches and symbols omitted; excluded files are not shown.
```

## Advice

```text
Supporting profiles selected the destination; qualified profiles also passed the evidence 
thresholds; absent profiles did not select the move; conflicts name alternative destinations. 
Advice consolidates the best candidate from each executed profile.
 Profiles share analysis-start evidence but apply their own weights and thresholds.
 Recommended (0):
 Review candidate (2):
   - file `crates/core/src/beta/scale.rs` → `crates/core/src/alpha` · supporting [greenfield] · 
   qualified [greenfield] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, no strict majority of executed 
     profiles provides qualifying support for this destination
   - symbol `unit` from `crates/core/src/beta/scale.rs` → `crates/core/src/alpha/parse.rs` · 
   supporting [greenfield] · qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the 
     configured minimum, the destination is not sufficiently stronger than the best alternative, 
     no strict majority of executed profiles provides qualifying support for this destination

```
