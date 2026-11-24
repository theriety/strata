# Unlace report — dupflat

7 files · 6 symbols · 4 edges
Snapshot `1f470932d8b366c8321a3b1e8ab7f0296fa9cbda2de3e802c0ff6b7bd4eea9bb`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (3)
  - cycle [violation] `dupflat/alpha.py`/`dupflat/beta.py` — 2-file import cycle; to break it,
    remove `dupflat.beta` (line 3) from `dupflat/alpha.py`, which carries 1 symbol reference
    (`alpha_step` -> `beta_step`) (exact)
  - polarity [violation] at `dupflat/service.py`, `dupflat/conftest.py`: production symbol `serve`
    (dupflat/service.py) depends on test code `sample_fixture` (dupflat/conftest.py)
  - testOnly [violation] at `dupflat/only_tests.py`, `helper`: Only tests in this repo use
    `helper`. It is public, so code outside this repo may use it too; check before moving or
    deleting it.

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline 0.0562; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.2562; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
