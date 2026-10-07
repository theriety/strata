# Strata report — test-heavy-ts

6 files · 9 symbols · 9 edges
Snapshot `b31193664d0bdc6a184849c647adb2583de241241a285368ccf08641ca32f56e`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (4)
  - testOnly [violation] at `src/adapters/anthropic/client.ts`, `closeClient`: Only tests in this
    repo use `closeClient`. It is public, so code outside this repo may use it too; check before
    moving or deleting it.
  - testOnly [violation] at `src/adapters/anthropic/client.ts`, `createClient`: Only tests in this
    repo use `createClient`. It is public, so code outside this repo may use it too; check before
    moving or deleting it.
  - testOnly [violation] at `src/adapters/anthropic/client.ts`, `sendMessage`: Only tests in this
    repo use `sendMessage`. It is public, so code outside this repo may use it too; check before
    moving or deleting it.
  - testOnly [violation] at `src/adapters/anthropic/models.ts`, `modelContext`: Only tests in this
    repo use `modelContext`. It is public, so code outside this repo may use it too; check before
    moving or deleting it.

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline 0.0770; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.2770; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
