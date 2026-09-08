# Strata report — nested-python

6 files · 12 symbols · 15 edges
Snapshot `908dd56719a4dd47cdb57e3c77b0a96aadd6b26edac973edda49371019b2da15`

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
Baseline score: 0.3844
Fewer than the requested candidates survived; the solution space converged.
Current layout is already optimal under this profile's search.
No candidates were produced.

greenfield — 1 candidate(s)
Baseline score: 0.5844
Fewer than the requested candidates survived; the solution space converged.

greenfield — candidate 1
  Score: 0.5844 → 0.3535; improvement +0.2310
  Proposed moves
    - Move file `app.py` → `geometry/app.py`
      Why: pulled toward `geometry/rectangle.py` — weight 2.0.
    - Move symbol `describe` from `geometry/rectangle.py` → `geometry/app.py`
    - Move type `Shape` from `geometry/shape.py` → `geometry/rectangle.py`
  Folder impacts
    `.`: 1 file(s) leaving · 0 entering
    `geometry`: 0 file(s) leaving · 1 entering
  Before
    nested-python/
    ├── app.py *
    └── geometry/
        ├── rectangle.py *
        │   └── symbol `describe` [to geometry/app.py]
        └── shape.py *
            └── type `Shape` [to geometry/rectangle.py]
  After
    nested-python/
    └── geometry/
        ├── app.py *
        │   └── symbol `describe` [from geometry/rectangle.py]
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
 Review candidate (3):
   - file `app.py` → `geometry` · supporting [greenfield] · qualified [] · absent [anchored] · 
   conflicts []
     review reasons: selected by only some executed profiles, destination evidence is below the 
     configured minimum, no strict majority of executed profiles provides qualifying support for 
     this destination
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
