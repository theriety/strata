# Unlace report — dup-ws-ts

9 files · 9 symbols · 11 edges
Snapshot `a3b98c858b287276b9296a39a60538966bf9a3971cbbf53f4455b920a8d38da9`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - capacity [violation] in `dup-ws-ts/atlas`: 2 domains holding files; the package cap is 1
    (`capacity.package`).
  - capacity [violation] in `dup-ws-ts/beacon/src/widget`: 2 folders holding files; the domain cap
    is 1 (`capacity.domain`).

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
Baseline score: 8.1769
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 8.3769
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 2 capacity cap(s).

greenfield — candidate 1
  Score: 8.3769 → 4.1954; improvement +4.1816
  Capacity remaining: 1 (0 at file level).
  Proposed moves
    - Move file `dup-ws-ts/atlas/src/agent/loop.ts` → `dup-ws-ts/atlas/src/core/loop.ts`
    - Move file `dup-ws-ts/atlas/src/agent/plan.ts` → `dup-ws-ts/atlas/src/core/plan.ts`
      Why: pulled toward `dup-ws-ts/atlas/src/core/alpha.ts` — weight 2.0.
  Folder impacts
    `dup-ws-ts/atlas/src/agent`: 2 file(s) leaving · 0 entering
    `dup-ws-ts/atlas/src/core`: 0 file(s) leaving · 2 entering
  Before
    dup-ws-ts/
    └── dup-ws-ts/
        └── atlas/
            └── src/
                └── agent/
                    ├── loop.ts *
                    └── plan.ts *
  After
    dup-ws-ts/
    └── dup-ws-ts/
        └── atlas/
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
   - file `dup-ws-ts/atlas/src/agent/loop.ts` → `dup-ws-ts/atlas/src/core` · supporting
   [greenfield] · qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination
   - file `dup-ws-ts/atlas/src/agent/plan.ts` → `dup-ws-ts/atlas/src/core` · supporting
   [greenfield] · qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination

```
