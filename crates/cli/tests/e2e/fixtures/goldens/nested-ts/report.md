# Unlace report — nested-ts

6 files · 12 symbols · 13 edges
Snapshot `2849870d3f7796de75e050917fb2c2619a7de38e1bcf5e5b07f2b244ba970498`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (1)
  - testOnly [violation] at `src/app.ts`, `run`: Only tests in this repo use `run`. It is public,
    so code outside this repo may use it too; check before moving or deleting it.

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
Baseline score: 0.1759
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.3759
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.3759 → 0.3144; improvement +0.0615
  Proposed moves
    - Move file `src/app.ts` → `src/geometry/app.ts`
      Why: pulled toward `src/geometry/rectangle.ts` — weight 2.0.
    - Move mirrored test file `src/__tests__/app.spec.ts` → `src/geometry/__tests__/app.spec.ts`;
      follows `src/app.ts`
    - Move symbol `describe` from `src/geometry/rectangle.ts` → `src/geometry/app.ts`
    - Move type `Shape` from `src/geometry/shape.ts` → `src/geometry/rectangle.ts`
  Folder impacts
    `src`: 1 file(s) leaving · 0 entering
    `src/__tests__`: 1 file(s) leaving · 0 entering
    `src/geometry`: 0 file(s) leaving · 1 entering
    `src/geometry/__tests__`: 0 file(s) leaving · 1 entering
  Before
    nested-ts/
    └── src/
        ├── __tests__/
        │   └── app.spec.ts *
        ├── app.ts *
        └── geometry/
            ├── rectangle.ts *
            │   └── symbol `describe` [to src/geometry/app.ts]
            └── shape.ts *
                └── type `Shape` [to src/geometry/rectangle.ts]
  After
    nested-ts/
    └── src/
        └── geometry/
            ├── __tests__/
            │   └── app.spec.ts *
            ├── app.ts *
            │   └── symbol `describe` [from src/geometry/rectangle.ts]
            ├── rectangle.ts *
            │   └── type `Shape` [from src/geometry/shape.ts]
            └── shape.ts *
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
 Review candidate (3):
   - file `src/app.ts` → `src/geometry` · supporting [greenfield] · qualified [] · absent
   [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, no strict majority of executed profiles provides qualifying support for
     this destination
   - symbol `describe` from `src/geometry/rectangle.ts` → `src/app.ts` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination
   - type `Shape` from `src/geometry/shape.ts` → `src/geometry/rectangle.ts` · supporting
   [greenfield] · qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, no strict majority of executed
     profiles provides qualifying support for this destination

```
