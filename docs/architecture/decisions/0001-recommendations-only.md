# ADR-0001: Recommendations only — strata never delivers or executes move scripts

- **Status:** Accepted
- **Date:** 2026-08-22

## Context

strata analyzes a repository's structure and proposes rearrangements: candidate
hierarchies of files into containers, each with a score breakdown, move
suggestions, and narration. Its output is consumed by a human who reads,
compares candidates, and decides what — if anything — to do.

The engine already computes everything an automated restructurer would need:
target paths, move lists, ordering constraints implied by the dependency DAG.
It would be technically easy to add an `apply` or `export` command that rewrites
`mod.rs`/`index.ts`/`__init__.py` trees or emits executable migration scripts.

## Decision

strata produces **recommendations only**. It never delivers move scripts, never
emits patch files, and never writes to or executes against the analyzed
repository. The product boundary is:

- Output is a static text/JSON dump rendered to stdout.
- Moves appear as advisory suggestions with rationale, for a human to act on.
- No command applies, exports, schedules, or executes changes to source.

This holds regardless of how complete the computed move data becomes; if a
future consumer wants automation, it integrates with strata from outside rather
than through a delivery feature inside strata.

## Consequences

- The read-only guarantee in the README and the engine's source-free design are
  product promises, not incidental limitations; renderer and CLI work must not
  grow write paths.
- Candidate comparison stays a presentation problem. Investment goes into making
  the static report legible enough to choose among candidates, not into
  interactive tooling.
- `pack` (splitting oversized files) remains a future capability of the same
  recommendation-only shape: it would propose splits, not perform them.
- Automated consumers that want to act on proposals build their own executor on
  top of strata's JSON output, outside this repository.
