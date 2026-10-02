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

| ADR | Title | Domain | Path | Status |
| --- | --- | --- | --- | --- |
| [ADR-1](decisions/product/adr-1-recommendations-only.md) | Recommendations only — strata never delivers or executes move scripts | product | `decisions/product/adr-1-recommendations-only.md` | Accepted |
| [ADR-5](decisions/relocation/adr-5-pass-start-dependencies-govern-symbol-moves.md) | Pass-start dependencies govern symbol moves | relocation | `decisions/relocation/adr-5-pass-start-dependencies-govern-symbol-moves.md` | Accepted |
| [ADR-6](decisions/relocation/adr-6-symbol-destination-guardrails.md) | Symbol destinations obey pass-start guardrails | relocation | `decisions/relocation/adr-6-symbol-destination-guardrails.md` | Accepted |
| [ADR-8](decisions/relocation/adr-8-physical-layout-governs-relocation-and-capacity.md) | Physical layout governs relocation and capacity | relocation | `decisions/relocation/adr-8-physical-layout-governs-relocation-and-capacity.md` | Accepted |
| [ADR-11](decisions/relocation/adr-11-relocation-ownership-scoring.md) | Relocation ownership and dependency-only scoring | relocation | `decisions/relocation/adr-11-relocation-ownership-scoring.md` | Accepted |
| [ADR-12](decisions/advice/adr-12-companion-owner-affinity.md) | Conservative companion-owner affinity | advice | `decisions/advice/adr-12-companion-owner-affinity.md` | Accepted |
| [ADR-14](decisions/advice/adr-14-profile-consensus.md) | Evidence-qualified profile consensus | advice | `decisions/advice/adr-14-profile-consensus.md` | Accepted |
| [ADR-16](decisions/reporting/adr-16-plain-text-reports-and-relocation-endpoints.md) | Plain-text reports and relocation endpoints | reporting | `decisions/reporting/adr-16-plain-text-reports-and-relocation-endpoints.md` | Accepted |
| [ADR-17](decisions/relocation/adr-17-relocations-stay-inside-their-package.md) | Relocations stay inside their package | relocation | `decisions/relocation/adr-17-relocations-stay-inside-their-package.md` | Accepted |
| [ADR-18](decisions/reporting/adr-18-move-endpoints-are-real-paths-under-see-through-roots.md) | Move endpoints are real paths under see-through source roots | reporting | `decisions/reporting/adr-18-move-endpoints-are-real-paths-under-see-through-roots.md` | Accepted |
| [ADR-19](decisions/analysis/adr-19-a-trait-impl-is-one-movable-unit.md) | A trait impl is one movable unit | analysis | `decisions/analysis/adr-19-a-trait-impl-is-one-movable-unit.md` | Accepted |
| [ADR-20](decisions/analysis/adr-20-re-exports-and-associated-calls-in-visibility.md) | Re-exports and associated calls in visibility findings | analysis | `decisions/analysis/adr-20-re-exports-and-associated-calls-in-visibility.md` | Accepted |
| [ADR-24](decisions/analysis/adr-24-visibility-never-narrows-below-the-language-ladder.md) | Visibility findings never narrow below the language's spellable scopes | analysis | `decisions/analysis/adr-24-visibility-never-narrows-below-the-language-ladder.md` | Accepted |
| [ADR-21](decisions/relocation/adr-21-a-colliding-file-move-folds-into-symbol-moves.md) | A colliding file move folds into symbol moves | relocation | `decisions/relocation/adr-21-a-colliding-file-move-folds-into-symbol-moves.md` | Accepted |
| [ADR-22](decisions/relocation/adr-22-test-symbols-are-pinned-by-polarity.md) | Test symbols are pinned by polarity | relocation | `decisions/relocation/adr-22-test-symbols-are-pinned-by-polarity.md` | Accepted |
