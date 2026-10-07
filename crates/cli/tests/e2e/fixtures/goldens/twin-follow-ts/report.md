# Strata report — twin-follow-ts

2 files · 2 symbols · 1 edges
Snapshot `1f2d68065bf97a7ab84b4f85a62c91f34b767247d5cc5cf99f964cd1e93d5af7`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (1)
  - testOnly [violation] at `src/api/openai.ts`, `complete`: Only tests in this repo use
    `complete`. It is public, so code outside this repo may use it too; check before moving or
    deleting it.

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline 0.4000; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.6000; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
