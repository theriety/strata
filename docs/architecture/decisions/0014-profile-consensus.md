# ADR-0014: Evidence-qualified profile consensus

- Status: `Accepted`
- Date: `2026-09-02`

## Context

Graph optimization can identify structurally admissible relocations without proving that a destination owns the moved declaration. A profile may prefer a move because of dependency proximity, naming, capacity, or another scored pressure even when several destinations remain plausible. Hiding every uncertain move would discard useful advice, while presenting every selected move with the same confidence would overstate what graph evidence can establish.

Anchored and greenfield are independently configurable parameter profiles, but they analyze the same discovered snapshot. Agreement between them is useful supporting evidence, not independent confirmation. Confidence therefore needs both destination-specific evidence within each profile and agreement across the profiles that were actually executed.

## Decision

Relocation advice is produced in two stages after candidate construction. Each profile first qualifies its best candidate using an immutable evidence index built from analysis-start placement. The engine then aggregates one selection per executed profile by relocation identity and destination.

Qualification uses six weighted signals and a derived ambiguity margin:

- `unique-owner` recognizes a sole explicit owner relationship in the destination;
- `role-affinity` measures token agreement with the destination, but can never qualify a move alone;
- `source-cohesion` measures whether destination support outweighs evidence that should keep the declaration with its source cluster;
- `destination-cohesion` measures support from the destination cluster and caps the contribution of a single neighbor;
- `producer-evidence` derives producer authority from explicit adapter-emitted owner affinities rather than passive consumption;
- `architectural-reach` favors a shared consumer ancestor over one consumer branch; and
- the ambiguity margin compares the proposed destination's evidence with the strongest conservative pass-start alternative.

All evidence comes from immutable analysis-start nodes, edges, affinities, files, and folders. Earlier accepted relocations cannot manufacture ownership or widen an ambiguity margin. Existing search, scoring, and hard structural guards remain unchanged. The guards are profile-independent; only evidence weights and qualification thresholds are profile parameters.

The ambiguity alternatives are the current source, incident production-neighbor homes, explicit owner homes, and the consumers' common physical folder. This evidence-bearing set deliberately over-approximates the optimizer's hard-eligible destination space rather than duplicating admission logic. A conservative alternative can lower confidence and send a proposal to review, but qualification cannot promote a move or remove the raw candidate. This one-way behavior avoids false confidence without creating a second, divergent search implementation.

Each profile defaults to `minimum-evidence = 0.60`, `minimum-structural = 0.50`, and `minimum-ambiguity-margin = 0.15`. The evidence score normalizes all six weighted signals. The structural score normalizes the same signals without role affinity, ensuring lexical agreement cannot satisfy the structural gate. The default weights are `unique-owner = 0.10`, `role-affinity = 0.10`, `source-cohesion = 0.25`, `destination-cohesion = 0.25`, `producer-evidence = 0.10`, and `architectural-reach = 0.20`. Values are independently configurable per profile.

Public advice has two groups:

- `Recommended` contains a relocation only when it passes the evidence and margin gates and a strict majority of all executed profiles select the same destination. For each relocation identity, a profile casts at most one vote from its first candidate. Two executed profiles therefore require two agreeing votes. A single-profile run requires one vote, but the move must still pass every qualification gate.
- `Review candidate` contains structurally safe proposals with weak evidence, an insufficient ambiguity margin, support from only some executed profiles, or conflicting profile destinations.

Ordinary non-moves remain absent; there is no public `Rejected` group. Classification explanations deterministically report the destination, per-profile assessments, evidence signals, ambiguity margin, supporting and qualifying profiles, absent profiles, conflicting destinations, and review reasons.

Analysis results use schema version 8. A top-level `advice` object carries `recommended` and `reviewCandidates`, while the raw per-profile candidate results remain available. Readers reject schema-version-7 and earlier saved results instead of inferring missing qualification evidence.

## Alternatives considered

**Treat every first-ranked profile move as recommended.** Rejected because objective improvement establishes structural preference, not ownership confidence.

**Use profile agreement without an evidence gate.** Rejected because profiles share correlated graph evidence and can agree on the same weak inference.

**Publish a rejected group.** Rejected because the complement includes every possible non-move and would imply that Strata had meaningfully evaluated an unbounded set of absent advice.

**Hide moves that do not reach recommendation confidence.** Rejected because partial support and destination ambiguity are actionable review information even when they are insufficient for an automatic recommendation.

**Let profiles configure structural guards.** Rejected because namespace, reach, dependency, file-role, and relocation-policy invariants define admissibility rather than preference.

**Recompute evidence after each accepted move.** Rejected because earlier optimization steps could manufacture authority for later moves and make explanations order-dependent.

## Consequences

Users receive a smaller high-confidence `Recommended` set and retain uncertain but structurally safe proposals under `Review candidate`. Each classification is explainable from language-neutral, pass-start evidence, and profile agreement is represented with its correlation limitation.

Profiles gain independently configurable qualification policy without acquiring the ability to bypass hard guards. Lexical names can support other evidence but cannot independently authorize relocation. More conservative thresholds may move advice from `Recommended` to `Review candidate`; they do not delete the underlying per-profile candidate.

Saved-result consumers must migrate to schema version 8 and read the top-level advice groups and their evidence explanations. The additional evidence indexing and cross-profile aggregation add deterministic analysis work after candidate construction but do not change candidate search, scoring, or structural findings.
