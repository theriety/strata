# ADR-0006: Symbol destinations obey pass-start guardrails

- **Status:** Accepted
- **Date:** 2026-08-28

## Context

Symbol-move scoring compares the cost of candidate placements, but some
destinations are invalid regardless of their score. A runtime symbol placed in
a type-only file changes that file's architectural role. A symbol placed in a
folder that did not already depend on all of the symbol's dependencies gives
that folder new directional coupling. The existing inbound reach guard covers
the complementary risk: moving a shared symbol must not give its dependants a
new destination-folder dependency.

These properties must be judged against one stable view of the analyzed
structure. Recomputing file roles or dependency permissions after accepted
moves would let one relocation authorize another during the same symbol pass.

## Decision

Symbol relocation uses immutable, pass-start admission guardrails before it
evaluates a candidate's score:

- A file is type-only when its pass-start residents contain at least one type
  and no runtime symbol. Runtime symbols cannot move into such a file. Types
  may, and mixed files remain eligible destinations.
- The outbound dependency envelope contains every non-self pass-start
  structural edge whose endpoints do not touch test-zone files, including
  edges whose scoring weight is zero. A symbol cannot move into a folder unless
  that folder already reaches every other folder reached by the symbol's
  outbound edges. Same-folder edges do not require a cross-folder permission.
- The inbound dependant reach guard remains in force. A symbol cannot move to a
  folder when doing so would make one of its existing dependant folders acquire
  a new dependency on the destination folder. Same-folder moves and hoists
  toward a folder enclosing the origin remain eligible.
- All three checks are hard admission rules. A rejected destination never
  reaches tentative placement or objective scoring, so no score improvement
  can override the structural boundary.

The guards derive only from semantic node kinds, structural edges, and the
assembled pass-start placement. They add no path-name heuristics, scoring
terms, coefficients, or public schema fields.

## Alternatives considered

**Score penalties.** Rejected because a penalty makes an invalid destination
negotiable: other objective terms can outvote it, and coefficient calibration
cannot express a hard structural boundary.

**Path conventions for type files or dependency layers.** Rejected because
repository-specific names such as `types` or `model` are weaker evidence than
the node kinds and edges already present in the snapshot.

**A mutable envelope updated after each accepted move.** Rejected because it
would make admissibility depend on move order and allow an earlier relocation
to authorize a later one within the same pass.

## Consequences

Destination admission is deterministic for the duration of a symbol pass and
cannot be weakened by scoring. Generated regressions for both new vetoes and
four positive controls pass, preserving valid runtime-to-mixed, type-to-type-
only, already-reachable-destination, and self-recursive moves. Pre-existing
single-consumer coverage remains green as well.

On the `ai` release analysis, anchored symbol moves fell from 241 to 162 and
greenfield moves fell from 262 to 181. The three confirmed invalid transitions
were absent after the change. A deterministic 12-row sample of removed moves
contained 11 appropriate removals, one possible removal, and no clearly
desirable removal, so the sample did not falsify the admission policy.

The implementation does not change path heuristics, objective scoring, or
public schemas. Formatting, clippy, and check gates are clean. The focused and
engine suites are green; `cargo test --workspace` retains one standing failure,
`cycle_span_witnesses_import_cycle_tearing`, unrelated to this decision.
