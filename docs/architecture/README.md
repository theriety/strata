# Architecture

[strata — Architecture](overview.md) describes the current structure: the
adapter → IR → engine pipeline, crate layering, snapshot contract, and
decomposition phases. [Deviations](deviations.md) records where implementation
departed from spec.

## Relocation advice

Relocation advice is produced in two stages. First, each executed parameter profile qualifies its best candidate using evidence from the immutable analysis-start graph. Second, the engine aggregates those qualified selections across profiles. Search, scoring, and structural admission happen before qualification; consensus cannot bypass namespace, reach, capacity, file-role, dependency-envelope, or relocation-policy guards.

The evidence model is language-neutral:

- **Unique owner** favors the destination containing the sole explicit owner relationship.
- **Role affinity** measures lexical agreement with the destination, but names never authorize a move by themselves.
- **Source cohesion** checks whether destination support outweighs the conceptual evidence lost by leaving the source.
- **Destination cohesion** requires broad support in the destination instead of treating one neighbor as ownership.
- **Producer evidence** derives from explicit adapter-emitted owner affinities rather than passive consumers.
- **Ambiguity margin** compares the proposed destination with the strongest conservative alternative supported by the analysis-start graph.
- **Architectural reach** favors the common ancestor of shared consumers over one consumer branch.

Hard structural guards are profile-independent. Evidence weights and thresholds are soft profile parameters, so profiles can express different architectural preferences without changing what is structurally admissible.

The alternative set is an intentional over-approximation built from the current source, incident production-neighbor homes, explicit owner homes, and the consumers' common physical folder. It is not a second execution of every optimizer guard. This one-way conservatism may demote advice for review, but cannot promote a move or suppress its raw proposal.

The public result has two groups. `Recommended` requires sufficient evidence and a strict majority of all executed profiles selecting the same destination; with two profiles this requires both. `Review candidate` preserves safe proposals with weak evidence, an insufficient ambiguity margin, partial profile support, or conflicting destinations. There is no public `Rejected` group because an ordinary non-move is simply absent. Profile agreement is supporting evidence, not independent proof, because every profile starts from the same graph.

## Decisions

| Document | Title | Status |
| --- | --- | --- |
| [ADR-0001](decisions/0001-recommendations-only.md) | Recommendations only — strata never delivers or executes move scripts | Accepted |
| [ADR-0005](decisions/0005-pass-start-dependencies-govern-symbol-moves.md) | Pass-start dependencies govern symbol moves | Accepted |
| [ADR-0006](decisions/0006-symbol-destination-guardrails.md) | Symbol destinations obey pass-start guardrails | Accepted |
| [ADR-0008](decisions/0008-physical-layout-governs-relocation-and-capacity.md) | Physical layout governs relocation and capacity | Accepted |
| [ADR-0011](decisions/0011-relocation-ownership-scoring.md) | Relocation ownership and dependency-only scoring | Accepted |
| [ADR-0012](decisions/0012-companion-owner-affinity.md) | Conservative companion-owner affinity | Accepted |
| [ADR-0013](decisions/0013-pinned-relocation-policies-and-test-mirrors.md) | Pinned relocation policies and exact test mirrors | Accepted |
| [ADR-0014](decisions/0014-profile-consensus.md) | Evidence-qualified profile consensus | Accepted |
| [ADR-0015](decisions/0015-consistent-reports-and-change-trees.md) | Consistent reports — shared content, verbosity, and before/after change trees | Accepted |
