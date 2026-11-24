# Unlace report — constellation-ts

28 files · 28 symbols · 48 edges
Snapshot `eaf518663e2a99630df5c38e7f020ca07d2c56a6d679d2b8a582a4f3b5a01a72`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (0)
  None.

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
Baseline score: 0.1330
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.3330
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.3330 → 0.1901; improvement +0.1429
  Proposed moves
    - Move file `src/util/clamp.ts` → `src/core/clamp.ts`
      Why: pulled toward `src/core/beta.ts` — weight 1.0.
    - Move file `src/util/hash.ts` → `src/model/hash.ts`
      Why: pulled toward `src/model/matrix.ts` — weight 1.0.
  Folder impacts
    `src/core`: 0 file(s) leaving · 1 entering
    `src/model`: 0 file(s) leaving · 1 entering
    `src/util`: 2 file(s) leaving · 0 entering
  Before
    constellation-ts/
    └── src/
        └── util/
            ├── clamp.ts *
            └── hash.ts *
  After
    constellation-ts/
    └── src/
        ├── core/
        │   └── clamp.ts *
        └── model/
            └── hash.ts *
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
   - file `src/util/clamp.ts` → `src/core` · supporting [greenfield] · qualified [greenfield] ·
   absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, no strict majority of executed
     profiles provides qualifying support for this destination
   - file `src/util/hash.ts` → `src/model` · supporting [greenfield] · qualified [greenfield] ·
   absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, no strict majority of executed
     profiles provides qualifying support for this destination

```
