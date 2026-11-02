# ADR-1: Recommendations only — strata never delivers or executes move scripts

📌

strata only recommends rearrangements for a human to act on; it never delivers move scripts, emits patches, or writes to or executes against the analyzed repository.

- Status: `Accepted`
- Date: `2026-08-22`

## 🎯 Motivation

A maintainer runs strata on a repository and receives candidate hierarchies of files into containers, each with a score breakdown, move suggestions, and narration. The maintainer reads the report, compares candidates, and decides what — if anything — to do.

The engine already computes everything an automated restructurer would need: target paths, move lists, and ordering constraints implied by the dependency DAG. It would be technically easy to add an `apply` or `export` command that rewrites `mod.rs`/`index.ts`/`__init__.py` trees or emits executable migration scripts. Without a firm product boundary, that ease invites a delivery feature that changes source instead of advising a human.

## 🧭 Context

strata analyzes a repository's structure and proposes rearrangements. Its output is consumed by a human reader. The README promises a read-only guarantee, and the engine is designed to be source-free.

## ✅ Decision

strata produces **recommendations only**. It never delivers move scripts, never emits patch files, and never writes to or executes against the analyzed repository. The product boundary is:

- Output is a static text/JSON dump rendered to stdout.
- Moves appear as advisory suggestions with rationale, for a human to act on.
- No command applies, exports, schedules, or executes changes to source.

This holds regardless of how complete the computed move data becomes; if a future consumer wants automation, it integrates with strata from outside rather than through a delivery feature inside strata.

## 🔀 Alternatives considered

**An `apply` or `export` command inside strata.** Rewriting `mod.rs`/`index.ts`/`__init__.py` trees or emitting executable migration scripts would be technically easy. Rejected because strata's product is the recommendation; automation, if wanted, integrates from outside strata rather than through a delivery feature inside it.

## ⚖️ Consequences

- The read-only guarantee in the README and the engine's source-free design are product promises, not incidental limitations; renderer and CLI work must not grow write paths.
- Candidate comparison stays a presentation problem. Investment goes into making the static report legible enough to choose among candidates, not into interactive tooling.
- `pack` (splitting oversized files) remains a future capability of the same recommendation-only shape: it would propose splits, not perform them.
- Automated consumers that want to act on proposals build their own executor on top of strata's JSON output, outside this repository.
