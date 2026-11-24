# Unlace report — polarity-leak

2 files · 2 symbols · 1 edges
Snapshot `60995d74e1b74b8955435fb79997d47896cfeae66e2e9209144d1f9554bb4d63`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (1)
  - polarity [violation] at `service.py`, `conftest.py`: production symbol `serve` (service.py)
    depends on test code `sample_fixture` (conftest.py)

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline 0.0250; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.2250; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
