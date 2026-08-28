# ADR-0005: Pass-start dependencies govern symbol moves

- **Status:** Accepted
- **Date:** 2026-08-27

## Context

Strata refines a candidate layout at two grains. `SymbolPass` drains symbols
from files one relocation at a time, and file-level polish relocates whole
files. Both loops must preserve the dependency evidence that made the candidate
safe when the loop began.

A per-step cycle check is insufficient for symbol draining. When a symbol and
one of its dependants begin in the same file, moving the dependant first removes
their same-file claim from the evolving placement. The depended-upon symbol can
then follow it into a destination that already reaches the original file. No
individual step needs to increase the current cycle count even though the
sequence has erased the evidence that should have barred the second move.

Cycle membership alone is also an incomplete budget. A tentative move can add a
dependency edge inside an existing non-trivial strongly connected component
without adding a vertex to that component. Counting only cyclic vertices admits
that extra coupling.

## Decision

`SymbolPass` admits relocations against an immutable pass-start dependency
baseline in addition to its evolving checks.

At the start of a pass, Strata builds the file dependency graph from the
assembly placement and structural edges. For every symbol, it records each
original file containing a dependant that shares the symbol's file. Before a
relocation is applied, Strata rejects any destination that could already reach
one of those original claimant files in the pass-start graph. Reachability is
transitive: an indirect path carries the same veto as a direct dependency.
Moving a dependant earlier in the pass therefore cannot erase the claim used to
judge a later symbol move.

The immutable admission rule operates alongside dynamic cycle checking. After
the baseline guard passes, Strata evaluates the tentative evolving graph to
defend against cycle shapes that were not represented by an original same-file
claimant.

Both symbol refinement and file-level polish use one two-dimensional cycle
budget:

- `vertices` counts members of non-trivial strongly connected components.
- `edges` counts dependency edges whose endpoints are in the same non-trivial
  strongly connected component.

The two values are computed from one condensation of the tentative graph. A
move is rejected when either value exceeds its current baseline; the comparison
is component-wise, not tuple or lexicographic. An accepted move updates both
baseline values. File-level polish reuses the tentative quotient for cycle
evaluation and scoring rather than rebuilding it for each measure.

TypeScript interface bodies and type-alias annotations contribute their member
type references to the dependency graph. Interface inheritance remains a
separate inheritance relationship and is not duplicated as a member type
reference. This parser coverage supplies the same-file claims that the
pass-start rule preserves.

## Alternatives considered

**Transactional sweep rollback.** Run the sequential pass and undo the whole
sweep if its aggregate graph is worse. Rejected because it discovers an
inadmissible sequence only after executing it, obscures which relocation broke
the invariant, and discards unrelated admissible moves from the same sweep.

**Grouped or atomic symbol moves.** Relocate mutually related symbols as one
unit. Rejected because it changes nomination and optimization semantics beyond
the dependency-safety requirement. Atomic grouping may be considered
separately, but safety cannot depend on it.

**Per-step evolving evidence alone.** Continue rebasing after each accepted
move and trust the current graph. Rejected because accepted moves can erase an
original claimant before the relocation it should veto is considered.

**A cyclic-vertex-only budget.** Treat unchanged cyclic membership as proof
that a tentative move is safe. Rejected because it permits additional edges
inside an existing cycle.

## Consequences

The TypeScript member-reference change was measured against the AI repository
at commit `7a9e70a9f6dddf5bc685728a0f23a0bb25f3ec6d`. The semantically classified
missing-member-reference residual fell from 13 to 0 in anchored mode and from
17 to 0 in greenfield mode. The gross 15 and 19 counts fell to 2 and 2; those
four observations were dependency-backed sequential-drain cases rather than
remaining parser blindness. The graph added genuine type dependencies, with no
unrelated or false dependency found by the audit.

With the pass-start guard enabled, every suggestion violating claimant
reachability was absent in both modes. In particular, the `OpenAIRequest` move
from `src/adapters/openai/tools.ts` to `src/adapters/openai/cache.ts` and the
`GOOGLE_DEFAULT_CONTENT_TYPE` move from
`src/generators/synthographers/adapters/google/codec.ts` to
`src/generators/synthographers/batch.ts` disappeared. The audit attributed 33
removals directly to the guard, found no unrelated change, and found no
legitimate relocation that lost all justification.

The edge dimension changes file-level polish where vertex membership cannot.
A tentative relocation of `src/adapters/anthropic/thinking.ts` left 45 cyclic
vertices unchanged but increased internal cyclic edges from 278 to 279, so the
move is rejected. Disabling only the edge comparison admits that witness. The
final repository audit found this to be the sole direct changed file placement;
other result differences were deterministic downstream repricing or cascade,
with no unrelated changes or false dependencies.

The additional admission and cycle checks run for every tentative relocation.
Their cost is accepted because they encode invariants that objective scoring
cannot safely trade away.
