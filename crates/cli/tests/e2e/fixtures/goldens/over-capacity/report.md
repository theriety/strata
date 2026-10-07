# Strata report — over-capacity

3 files · 375 symbols · 0 edges
Snapshot `3f214d4ce8d116f9db703133f34f3be610b97a013f46bd10049050eb5bbdeaa6`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - capacity [violation] file `huge.py` holds 300 against a cap of 250
  - capacity [violation] file `pkg/nested/deep/huge_nested.py` holds 300 against a cap of 250

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline 1.4095; search converged; current layout breaks 2 cap(s))
greenfield — 0 candidates (baseline 1.6095; search converged; current layout breaks 2 cap(s))
```

## Advice

```text
  None.
```
