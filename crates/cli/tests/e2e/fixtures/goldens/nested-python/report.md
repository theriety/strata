# Unlace report — nested-python

6 files · 12 symbols · 15 edges
Snapshot `a68168f1734917d3ddabff66885f27988a64cdfbbc3fdcb09bd9459a640b0760`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (1)
  - testOnly [violation] at `app.py`, `run`: Only tests in this repo use `run`. It is public, so
    code outside this repo may use it too; check before moving or deleting it.

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
Baseline score: 0.1984
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.3984
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.3984 → 0.3455; improvement +0.0529
  Proposed moves
    - Move symbol `describe` from `geometry/rectangle.py` → `app.py`
    - Move type `Shape` from `geometry/shape.py` → `geometry/rectangle.py`
  Folder impacts
    No files enter or leave a folder.
  Before
    nested-python/
    ├── app.py *
    └── geometry/
        ├── rectangle.py *
        │   └── symbol `describe` [to app.py]
        └── shape.py *
            └── type `Shape` [to geometry/rectangle.py]
  After
    nested-python/
    ├── app.py *
    │   └── symbol `describe` [from geometry/rectangle.py]
    └── geometry/
        ├── rectangle.py *
        │   └── type `Shape` [from geometry/shape.py]
        └── shape.py *
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
   - symbol `describe` from `geometry/rectangle.py` → `app.py` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination
   - type `Shape` from `geometry/shape.py` → `geometry/rectangle.py` · supporting [greenfield] ·
   qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, no strict majority of executed
     profiles provides qualifying support for this destination

```
