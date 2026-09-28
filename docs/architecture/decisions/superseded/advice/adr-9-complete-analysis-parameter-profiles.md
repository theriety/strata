> **Status:** Superseded
>
> **Superseded by:** [ADR-11 — Relocation ownership and dependency-only scoring](../../relocation/adr-11-relocation-ownership-scoring.md)
>
> **What changed:** Partial change: analysis results advance from schema version 4 to version 5 so profile scoring can carry the dependency-only objective term.
>
> superseded-by: adr-11

# ADR-9: Complete analysis parameter profiles

- Status: `Accepted`
- Date: `2026-08-30`

## Context

Anchored and greenfield analyses use the same discovered dependency graph but may express different structural intent. Treating them as a selector over a small subset of global coefficients leaves capacity, dependency pricing, solver limits, diversification, test policy, candidate count, and seed coupled even when a user needs those policies to differ. It also makes a single shared current score or finding list misleading when profiles can use different caps and edge prices.

Same-file dependencies require a related distinction. A runtime declaration and a type declaration can both have external consumers, but a type colocated with its primary runtime consumer often defines that consumer's contract. Pricing every same-file dependency exactly like an external reference can therefore recommend separating a type from the function that owns its meaning merely to satisfy weaker passthrough consumers.

Saved analysis results and human reports must preserve those differences without duplicating findings that are genuinely identical. Configuration migration must also fail explicitly rather than silently combining old global policy with new per-profile policy.

## Decision

Anchored and greenfield are complete, independently configurable parameter profiles executed against one discovered snapshot. Each profile owns its candidate count, seed, capacity caps, objective coefficients, dependency and same-file weights, solver budget, diversity settings, and test policy. Adapter discovery and process-wide parallelism remain global.

The anchored defaults use nonzero path and anchor coefficients. The greenfield defaults set both coefficients to zero, but explicit nonzero greenfield values are honored. Selecting profiles does not alter their parameters. Generic command-line candidate and seed overrides apply to every selected profile.

Configuration accepts selected profile names and process-wide jobs under `[analysis]`, with the complete policy under `[profiles.anchored]` and `[profiles.greenfield]`. Legacy global analysis-policy keys and sections are rejected as unknown configuration. No compatibility aliases are accepted in the configuration file.

Dependency pricing classifies every edge from immutable analysis-start placement:

- an edge whose endpoints begin in different files uses its ordinary dependency-kind weight;
- a same-file edge touching any type uses the selected profile's `same-file-type` multiplier; and
- every other same-file edge uses the selected profile's `same-file-symbol` multiplier.

The default multipliers are `3.0` for type-touching edges and `1.0` for runtime-only edges. Each profile may configure either value down to `1.0`. The immutable classification is reused throughout scoring, candidate ordering, score breakdowns, and symbol-move deltas; accepted relocations cannot reclassify later edges.

Analysis results use schema version 4. The top-level current value contains the one shared current tree and `sharedFindings`. Each executed profile contains its complete effective parameters, profile-specific current score and breakdown, standing, hard capacity-break count, `uniqueFindings`, candidates, and diversity result. A finding is shared only when its complete serialized content is identical in both executed profiles. Each profile's unique list excludes that exact intersection. When only one profile executes, the shared list is empty and every finding remains profile-specific. All finding lists are sorted and deduplicated deterministically.

Capacity-break counts include every hard capacity finding applicable to the profile, including findings carried in the shared list. Borderline findings remain visible but do not count as hard breaks. Human output presents shared findings before profile-specific findings, labels anchored and greenfield as parameter profiles, shows effective parameters, and compares each candidate gain only with its own profile baseline.

The CLI retains `--mode` as a compatibility selector because scripts and saved-result navigation already use the anchored and greenfield names. The flag selects parameter profiles; it is not a configuration model and does not restore the removed global policy.

## Alternatives considered

**Keep global policy and vary only objective coefficients.** Rejected because profile-specific capacity, test, solver, and dependency decisions would remain impossible, while current findings would still be ambiguous.

**Clone the discovered snapshot per profile.** Rejected because discovery is not analysis policy. Sharing the immutable snapshot preserves identical input evidence and avoids duplicated adapter work.

**Apply same-file affinity only during symbol relocation.** Rejected because candidate ordering, score breakdowns, and reported move deltas would then disagree about the cost of the same dependency.

**Reclassify edges after each accepted move.** Rejected because earlier moves could manufacture or erase affinity for later moves. Admission and pricing must be governed by pass-start evidence.

**Share findings by identity fields or display text.** Rejected because profiles can produce superficially similar findings with different severities, paths, cuts, or prices. Only complete serialized equality is safe to deduplicate.

**Accept legacy sections as aliases.** Rejected because precedence between global and profile-local values would be unclear and could silently run a different policy than the user intended.

## Consequences

Users can tune anchored and greenfield independently while comparing them over exactly the same repository snapshot. The effective parameters recorded with each result make those comparisons reproducible and explain why current scores or findings differ.

Type declarations default to stronger affinity with their analysis-start file, reducing moves that separate a contract from its primary consumer. Runtime behavior keeps the previous `1.0` same-file multiplier. Stronger affinity is policy rather than a hard placement rule, and either profile can reduce it to `1.0` when a different tradeoff is intended.

Saved-result consumers must migrate from schema version 3's `modes` and shared current evaluation to version 4's `profiles`, shared tree and findings, and per-profile current state. Existing result readers reject unsupported versions instead of guessing. Configuration files using removed global keys fail until migrated.

Finding generation, scoring, solver execution, and test classification may now differ between profiles. Exact-equality sharing prevents duplicate output without hiding those differences. Human reports grow an effective-parameter section, but every score and gain is attributable to the policy that produced it.
