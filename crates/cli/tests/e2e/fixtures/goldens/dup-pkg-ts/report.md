# Unlace report — dup-pkg-ts

11 files · 11 symbols · 11 edges
Snapshot `53567d8884bf39711e1ac08a615576771323d3d1f5d90b01392bb9050882a0f8`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - capacity [violation] in `beacon/src/widget`: 3 sub-folders directly inside; the domain cap is
    2 (`capacity.domain`).
  - capacity [violation] in `dup-pkg-ts`: 2 domains holding files; the package cap is 1
    (`capacity.package`).

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
Baseline score: 6.2103
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 6.4103
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

greenfield — candidate 1
  Score: 6.4103 → 4.2180; improvement +2.1923
  Capacity remaining: 1 (0 at file level).
  Proposed moves
    - Move file `dup-pkg-ts/src/agent/loop.ts` → `dup-pkg-ts/src/core/loop.ts`
    - Move file `dup-pkg-ts/src/agent/plan.ts` → `dup-pkg-ts/src/core/plan.ts`
      Why: pulled toward `dup-pkg-ts/src/core/alpha.ts` — weight 2.0.
  Folder impacts
    `dup-pkg-ts/src/agent`: 2 file(s) leaving · 0 entering
    `dup-pkg-ts/src/core`: 0 file(s) leaving · 2 entering
  Before
    dup-pkg-ts/
    └── dup-pkg-ts/
        └── src/
            └── agent/
                ├── loop.ts *
                └── plan.ts *
  After
    dup-pkg-ts/
    └── dup-pkg-ts/
        └── src/
            └── core/
                ├── loop.ts *
                └── plan.ts *
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
   - file `dup-pkg-ts/src/agent/loop.ts` → `dup-pkg-ts/src/core` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination
   - file `dup-pkg-ts/src/agent/plan.ts` → `dup-pkg-ts/src/core` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination

```
