# Unlace report — mixed-root-ts

12 files · 12 symbols · 11 edges
Snapshot `451050929fc795aedc83f73e3c197d546c55752d7630ca1f4fc9a17bb10a0100`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - capacity [violation] in `.`: 5 entries (files and subfolders); the folder cap is 3
    (`capacity.folder`).
  - capacity [violation] in `atlas/src/core`: 4 entries (files and subfolders); the folder cap is
    3 (`capacity.folder`).

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

anchored — 1 candidate(s)
Baseline score: 4.2038
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

anchored — candidate 1
  Score: 4.2038 → 3.0994; improvement +1.1044
  Capacity remaining: 1 (0 at file level).
  Proposed moves
    - Move file `atlas/src/core/beta.ts` → `atlas/src/core/beta/beta.ts`
    - Move file `atlas/src/core/delta.ts` → `atlas/src/core/beta/delta.ts`
    - Move file `atlas/src/core/gamma.ts` → `atlas/src/core/beta/gamma.ts`
      Why: relieves `atlas/src/core`, which has 4 entries (files and subfolders) against the
      folder cap of 3 (`capacity.folder`).
  Folder impacts
    `atlas/src/core`: 3 file(s) leaving · 0 entering
    `atlas/src/core/beta`: 0 file(s) leaving · 3 entering
  Before
    mixed-root-ts/
    └── atlas/
        └── src/
            └── core/
                ├── beta.ts *
                ├── delta.ts *
                └── gamma.ts *
  After
    mixed-root-ts/
    └── atlas/
        └── src/
            └── core/
                └── beta/
                    ├── beta.ts *
                    ├── delta.ts *
                    └── gamma.ts *
  * File moved or its symbol contents changed.
  Unchanged branches and symbols omitted; excluded files are not shown.

greenfield — 1 candidate(s)
Baseline score: 4.4038
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

greenfield — candidate 1
  Score: 4.4038 → 2.9851; improvement +1.4187
  Capacity remaining: 1 (0 at file level).
  Proposed moves
    - Move file `atlas/src/core/beta.ts` → `atlas/src/core/beta/beta.ts`
    - Move file `atlas/src/core/delta.ts` → `atlas/src/core/beta/delta.ts`
    - Move file `atlas/src/core/gamma.ts` → `atlas/src/core/beta/gamma.ts`
      Why: relieves `atlas/src/core`, which has 4 entries (files and subfolders) against the
      folder cap of 3 (`capacity.folder`).
  Folder impacts
    `atlas/src/core`: 3 file(s) leaving · 0 entering
    `atlas/src/core/beta`: 0 file(s) leaving · 3 entering
  Before
    mixed-root-ts/
    └── atlas/
        └── src/
            └── core/
                ├── beta.ts *
                ├── delta.ts *
                └── gamma.ts *
  After
    mixed-root-ts/
    └── atlas/
        └── src/
            └── core/
                └── beta/
                    ├── beta.ts *
                    ├── delta.ts *
                    └── gamma.ts *
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
   - file `atlas/src/core/beta.ts` → `atlas/src/core/beta` · supporting [anchored, greenfield] ·
   qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `atlas/src/core/delta.ts` → `atlas/src/core/beta` · supporting [anchored, greenfield] ·
   qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `atlas/src/core/gamma.ts` → `atlas/src/core/beta` · supporting [anchored, greenfield] ·
   qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination

```
