# Unlace report — dup-split-ts

11 files · 11 symbols · 12 edges
Snapshot `8e05d78407a915f4fc3b666a524bee7a5b8ad04a234701d48ca1b4864dc9525e`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - capacity [violation] in `atlas/foo`: 4 entries (files and subfolders); the folder cap is 3
    (`capacity.folder`).
  - capacity [violation] in `dup-split-ts/atlas/src/core`: 4 entries (files and subfolders); the
    folder cap is 3 (`capacity.folder`).

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
Baseline score: 2.8270
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

anchored — candidate 1
  Score: 2.8270 → 0.6047; improvement +2.2222
  Capacity remaining: 0 (0 at file level).
  Proposed moves
    - Move file `atlas/foo/b1.ts` → `atlas/foo/b/b1.ts`
    - Move file `atlas/foo/b2.ts` → `atlas/foo/b/b2.ts`
      Why: relieves `atlas/foo`, which has 4 entries (files and subfolders) against the folder cap
      of 3 (`capacity.folder`).
    - Move file `dup-split-ts/atlas/src/core/beta.ts` → `dup-split-ts/atlas/src/core/beta/beta.ts`
    - Move file `dup-split-ts/atlas/src/core/delta.ts` →
      `dup-split-ts/atlas/src/core/beta/delta.ts`
    - Move file `dup-split-ts/atlas/src/core/gamma.ts` →
      `dup-split-ts/atlas/src/core/beta/gamma.ts`
      Why: relieves `dup-split-ts/atlas/src/core`, which has 4 entries (files and subfolders)
      against the folder cap of 3 (`capacity.folder`).
  Folder impacts
    `atlas/foo`: 2 file(s) leaving · 0 entering
    `atlas/foo/b`: 0 file(s) leaving · 2 entering
    `dup-split-ts/atlas/src/core`: 3 file(s) leaving · 0 entering
    `dup-split-ts/atlas/src/core/beta`: 0 file(s) leaving · 3 entering
  Before
    dup-split-ts/
    ├── atlas/
    │   └── foo/
    │       ├── b1.ts *
    │       └── b2.ts *
    └── dup-split-ts/
        └── atlas/
            └── src/
                └── core/
                    ├── beta.ts *
                    ├── delta.ts *
                    └── gamma.ts *
  After
    dup-split-ts/
    ├── atlas/
    │   └── foo/
    │       └── b/
    │           ├── b1.ts *
    │           └── b2.ts *
    └── dup-split-ts/
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
Baseline score: 3.0270
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

greenfield — candidate 1
  Score: 3.0270 → 0.2545; improvement +2.7724
  Capacity remaining: 0 (0 at file level).
  Proposed moves
    - Move file `atlas/foo/b1.ts` → `atlas/foo/b/b1.ts`
    - Move file `atlas/foo/b2.ts` → `atlas/foo/b/b2.ts`
      Why: relieves `atlas/foo`, which has 4 entries (files and subfolders) against the folder cap
      of 3 (`capacity.folder`).
    - Move file `dup-split-ts/atlas/src/core/beta.ts` → `dup-split-ts/atlas/src/core/beta/beta.ts`
    - Move file `dup-split-ts/atlas/src/core/delta.ts` →
      `dup-split-ts/atlas/src/core/beta/delta.ts`
    - Move file `dup-split-ts/atlas/src/core/gamma.ts` →
      `dup-split-ts/atlas/src/core/beta/gamma.ts`
      Why: relieves `dup-split-ts/atlas/src/core`, which has 4 entries (files and subfolders)
      against the folder cap of 3 (`capacity.folder`).
  Folder impacts
    `atlas/foo`: 2 file(s) leaving · 0 entering
    `atlas/foo/b`: 0 file(s) leaving · 2 entering
    `dup-split-ts/atlas/src/core`: 3 file(s) leaving · 0 entering
    `dup-split-ts/atlas/src/core/beta`: 0 file(s) leaving · 3 entering
  Before
    dup-split-ts/
    ├── atlas/
    │   └── foo/
    │       ├── b1.ts *
    │       └── b2.ts *
    └── dup-split-ts/
        └── atlas/
            └── src/
                └── core/
                    ├── beta.ts *
                    ├── delta.ts *
                    └── gamma.ts *
  After
    dup-split-ts/
    ├── atlas/
    │   └── foo/
    │       └── b/
    │           ├── b1.ts *
    │           └── b2.ts *
    └── dup-split-ts/
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
 Review candidate (5):
   - file `atlas/foo/b1.ts` → `atlas/foo/b` · supporting [anchored, greenfield] · qualified [] ·
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `atlas/foo/b2.ts` → `atlas/foo/b` · supporting [anchored, greenfield] · qualified [] ·
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `dup-split-ts/atlas/src/core/beta.ts` → `dup-split-ts/atlas/src/core/beta` · supporting
   [anchored, greenfield] · qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `dup-split-ts/atlas/src/core/delta.ts` → `dup-split-ts/atlas/src/core/beta` · supporting
   [anchored, greenfield] · qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination
   - file `dup-split-ts/atlas/src/core/gamma.ts` → `dup-split-ts/atlas/src/core/beta` · supporting
   [anchored, greenfield] · qualified [] · absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is
     insufficient, the destination is not sufficiently stronger than the best alternative, no
     strict majority of executed profiles provides qualifying support for this destination

```
