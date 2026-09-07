# Strata report — relief-nest-py

24 files · 24 symbols · 22 edges
Snapshot `7ebf6d1c547dcf9580fbaf51fe0fb020678583078c443f5f6531987a5dfe21be`

## Structural findings

```text
Shared findings (1)
  - capacity [violation] at container ancestry `relief-nest-py`, `hub`: folder 
    `relief-nest-py/hub` holds 24 against a cap of 20

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
Baseline score: 0.6728
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 1 capacity cap(s).

anchored — candidate 1
  Score: 0.6728 → 0.4250; improvement +0.2478
  Capacity remaining: 0 (0 at file level).
  Proposed moves
    - Move file `hub/ingest_00.py` → `hub/ingest/ingest_00.py`
    - Move file `hub/ingest_01.py` → `hub/ingest/ingest_01.py`
    - Move file `hub/ingest_02.py` → `hub/ingest/ingest_02.py`
    - Move file `hub/ingest_03.py` → `hub/ingest/ingest_03.py`
    - Move file `hub/ingest_04.py` → `hub/ingest/ingest_04.py`
    - Move file `hub/ingest_05.py` → `hub/ingest/ingest_05.py`
    - Move file `hub/ingest_06.py` → `hub/ingest/ingest_06.py`
    - Move file `hub/ingest_07.py` → `hub/ingest/ingest_07.py`
    - Move file `hub/ingest_08.py` → `hub/ingest/ingest_08.py`
    - Move file `hub/ingest_09.py` → `hub/ingest/ingest_09.py`
    - Move file `hub/ingest_10.py` → `hub/ingest/ingest_10.py`
    - Move file `hub/ingest_11.py` → `hub/ingest/ingest_11.py`
      Why: relieves `hub`, which holds 24 against a cap of 20.
  Folder impacts
    `hub`: 12 file(s) leaving · 0 entering
    `hub/ingest`: 0 file(s) leaving · 12 entering
  Before
    relief-nest-py/
    └── hub/
        ├── ingest_00.py *
        ├── ingest_01.py *
        ├── ingest_02.py *
        ├── ingest_03.py *
        ├── ingest_04.py *
        ├── ingest_05.py *
        ├── ingest_06.py *
        ├── ingest_07.py *
        ├── ingest_08.py *
        ├── ingest_09.py *
        ├── ingest_10.py *
        └── ingest_11.py *
  After
    relief-nest-py/
    └── hub/
        └── ingest/
            ├── ingest_00.py *
            ├── ingest_01.py *
            ├── ingest_02.py *
            ├── ingest_03.py *
            ├── ingest_04.py *
            ├── ingest_05.py *
            ├── ingest_06.py *
            ├── ingest_07.py *
            ├── ingest_08.py *
            ├── ingest_09.py *
            ├── ingest_10.py *
            └── ingest_11.py *
  * File moved or its symbol contents changed.
  Unchanged branches and symbols omitted; excluded files are not shown.

greenfield — 1 candidate(s)
Baseline score: 0.8728
Fewer than the requested candidates survived; the solution space converged.
Current layout violates 1 capacity cap(s).

greenfield — candidate 1
  Score: 0.8728 → 0.0250; improvement +0.8478
  Capacity remaining: 0 (0 at file level).
  Proposed moves
    - Move file `hub/ingest_00.py` → `hub/ingest/ingest_00.py`
    - Move file `hub/ingest_01.py` → `hub/ingest/ingest_01.py`
    - Move file `hub/ingest_02.py` → `hub/ingest/ingest_02.py`
    - Move file `hub/ingest_03.py` → `hub/ingest/ingest_03.py`
    - Move file `hub/ingest_04.py` → `hub/ingest/ingest_04.py`
    - Move file `hub/ingest_05.py` → `hub/ingest/ingest_05.py`
    - Move file `hub/ingest_06.py` → `hub/ingest/ingest_06.py`
    - Move file `hub/ingest_07.py` → `hub/ingest/ingest_07.py`
    - Move file `hub/ingest_08.py` → `hub/ingest/ingest_08.py`
    - Move file `hub/ingest_09.py` → `hub/ingest/ingest_09.py`
    - Move file `hub/ingest_10.py` → `hub/ingest/ingest_10.py`
    - Move file `hub/ingest_11.py` → `hub/ingest/ingest_11.py`
      Why: relieves `hub`, which holds 24 against a cap of 20.
  Folder impacts
    `hub`: 12 file(s) leaving · 0 entering
    `hub/ingest`: 0 file(s) leaving · 12 entering
  Before
    relief-nest-py/
    └── hub/
        ├── ingest_00.py *
        ├── ingest_01.py *
        ├── ingest_02.py *
        ├── ingest_03.py *
        ├── ingest_04.py *
        ├── ingest_05.py *
        ├── ingest_06.py *
        ├── ingest_07.py *
        ├── ingest_08.py *
        ├── ingest_09.py *
        ├── ingest_10.py *
        └── ingest_11.py *
  After
    relief-nest-py/
    └── hub/
        └── ingest/
            ├── ingest_00.py *
            ├── ingest_01.py *
            ├── ingest_02.py *
            ├── ingest_03.py *
            ├── ingest_04.py *
            ├── ingest_05.py *
            ├── ingest_06.py *
            ├── ingest_07.py *
            ├── ingest_08.py *
            ├── ingest_09.py *
            ├── ingest_10.py *
            └── ingest_11.py *
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
 Review candidate (12):
   - file `hub/ingest_00.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_01.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_02.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_03.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_04.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_05.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_06.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_07.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_08.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_09.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_10.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination
   - file `hub/ingest_11.py` → `hub/ingest` · supporting [anchored, greenfield] · qualified [] · 
   absent [] · conflicts []
     review reasons: destination evidence is below the configured minimum, structural evidence is 
     insufficient, the destination is not sufficiently stronger than the best alternative, no 
     strict majority of executed profiles provides qualifying support for this destination

```
