# Unlace report — cyclic-oversized

3 files · 3 symbols · 3 edges
Snapshot `99d887a0671a8e39765d9a9fd111cf291d4d678a7849ca13bdd6ab14194f6b0f`
All findings and moves are suggestions. Applying them may break the build; use your own judgement.

## Structural findings

```text
Shared findings (1)
  - cycle [violation] `alpha.ts`/`beta.ts`/`gamma.ts` — 3-file import cycle; to break it, remove
    `./beta` (line 1) from `alpha.ts`, which carries 1 symbol reference (`stepAlpha` ->
    `stepBeta`) (exact)

anchored profile-specific findings (0)
  None.

greenfield profile-specific findings (0)
  None.
```

## Candidate layouts

```text
anchored — 0 candidates (baseline -0.0750; search converged; current layout already optimal)
greenfield — 0 candidates (baseline 0.1250; search converged; current layout already optimal)
```

## Advice

```text
  None.
```
