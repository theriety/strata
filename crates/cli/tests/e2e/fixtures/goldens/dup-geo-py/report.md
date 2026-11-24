# Unlace report — dup-geo-py

6 files · 12 symbols · 8 edges
Snapshot `89f65b0d94db3de813e1d2c9f7f8dfea00a5cd3b8b01a32127331ede583cf750`
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
Baseline score: -0.0272
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.1728
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.1728 → 0.1445; improvement +0.0283
  Proposed moves
    - Move type `Shape` from `dup-geo-py/geometry/shape.py` → `dup-geo-py/geometry/rectangle.py`
  Folder impacts
    No files enter or leave a folder.
  Before
    dup-geo-py/
    └── dup-geo-py/
        └── geometry/
            ├── rectangle.py *
            └── shape.py *
                └── type `Shape` [to dup-geo-py/geometry/rectangle.py]
  After
    dup-geo-py/
    └── dup-geo-py/
        └── geometry/
            ├── rectangle.py *
            │   └── type `Shape` [from dup-geo-py/geometry/shape.py]
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
 Review candidate (1):
   - type `Shape` from `dup-geo-py/geometry/shape.py` → `dup-geo-py/geometry/rectangle.py` ·
   supporting [greenfield] · qualified [] · absent [anchored] · conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the
     configured minimum, structural evidence is insufficient, the destination is not sufficiently
     stronger than the best alternative, no strict majority of executed profiles provides
     qualifying support for this destination

```
