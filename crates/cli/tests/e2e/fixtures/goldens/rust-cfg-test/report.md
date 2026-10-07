# Strata report — rust-cfg-test

1 files · 4 symbols · 2 edges
Snapshot `a9bbb4a2c82ab025b4720392bf0c1403ffe71a1707e77f28c2d7efd48292a935`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (2)
  - testOnly [violation] at `src/lib.rs`, `double`: Only tests in this repo use `double`. It is
    public, so code outside this repo may use it too; check before moving or deleting it.
  - testOnly [violation] at `src/lib.rs`, `halve`: Only tests in this repo use `halve`. It is
    public, so code outside this repo may use it too; check before moving or deleting it.

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline -0.1375; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.0625; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
