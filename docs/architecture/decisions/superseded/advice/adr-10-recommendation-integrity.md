> **Status:** Superseded
>
> **Superseded by:** [ADR-11 — Relocation ownership and dependency-only scoring](../../relocation/adr-11-relocation-ownership-scoring.md)
>
> **What changed:** Partial change: dependency-only destinations are governed by a profile objective charge, while shared ownership is protected by an immutable consumer-branch guard.
>
> superseded-by: adr-11

# ADR-10: Recommendations require structural and scoring integrity

- **Status:** Accepted
- **Date:** 2026-08-30

## Context

Strata presents restructuring candidates as improvements under a selected
parameter profile. That promise is broken when an infeasible current layout
causes a higher-scoring fallback candidate to be offered, or when capacity
findings and the capacity objective measure different structures. Because lower
scores are better, a score regression cannot become a gain merely because the
current layout violates a hard constraint.

Symbol relocation has a related evidence problem. An undirected relationship
can pull a declaration toward a file containing something the declaration uses,
even while a consumer that owns the declaration remains in the source file.
Qualified test filenames can also be mistaken for production files, distinct
declarations can be brought into one naming scope, and import narration can omit
the source consumer that must import the moved declaration. Each defect makes a
syntactically plausible suggestion misrepresent the structure a maintainer
would create.

These guarantees must come from profile scoring, node kinds, directed edges,
and immutable analysis-start placement. Repository-specific names and
application concepts are not portable architectural evidence.

## Decision

A parameter profile offers only candidates whose total score is strictly lower
than that profile's current-tree score. Current-layout infeasibility remains a
finding and does not admit a worse-scoring fallback. Candidate improvement is
always computed from the same profile baseline, so every offered value is
positive.

Capacity scoring uses the complete capacity policy and the same measurements as
capacity findings. File capacity in both paths sums production source lines and
excludes nodes with test polarity. The remaining measures are immediate
physical folder entries, folders bound to domains, domains bound to packages,
and packages bound to package groups. Each level uses its own configured cap.
The current tree, tentative symbol placements, and completed candidates all use
this calculation.

The TypeScript adapter treats a supported `.ts` or `.tsx` filename as a test
when any dot-delimited qualifier after its base name is exactly `spec` or
`test`. A path segment exactly equal to `__tests__` also marks a test. Partial
words do not match. This classifies qualified variants without encoding a
project's integration-test vocabulary.

Symbol relocation adds two immutable, pass-start admission rules:

- Directed incoming edges identify consumers of a declaration. If the source
  file contained a consumer at pass start, a destination is admissible only
  when it also contained a consumer at pass start. This ownership evidence is
  immutable even if an earlier relocation moves the source consumer. Outgoing
  dependencies do not constitute ownership. A declaration with no pass-start
  source-file consumer may still move toward a dependency, subject to the other
  relocation guardrails.
- A destination may not contain two distinct declarations with the same name.
  The check includes pass-start residents and earlier accepted arrivals in the
  current symbol pass.

Move narration counts every surviving file that must begin importing the moved
declaration, including its source file. Human output describes positive changes
as gains, zero changes as no improvement, and negative changes as regressions.
Only an improving candidate may receive an adoption recommendation. Defensive
rendering preserves truthful wording even when a result is supplied by an
external or older producer.

The rules change private analysis and rendering behavior only. Public
configuration, IR, and result schemas remain unchanged.

## Alternatives considered

**Offer the least-bad candidate when the current layout is infeasible.**
Rejected because it makes hard findings silently override the documented scalar
objective and turns a negative score change into an apparent improvement. A
profile with no improving candidate must say so.

**Use the folder cap as a proxy for all capacity pressure.** Rejected because a
proxy can disagree with level-specific findings and configured policy. Every
capacity level must price the same measurement used to report its breach.

**Treat incident edges as undirected ownership evidence.** Rejected because a
declaration's dependencies explain what it needs, while incoming consumers
explain where it is used. Conflating those directions can move behavior or a
contract away from its owner.

**Infer destination roles from filenames.** Rejected because names such as
`types`, `constants`, or `helpers` are conventions rather than semantic
evidence and do not generalize across languages or repositories.

**Rely on compilation to detect declaration-name collisions.** Rejected
because Strata should not recommend a placement that is already known to create
an invalid destination scope.

**Require users to configure every qualified test pattern.** Rejected because
dot-qualified `spec` and `test` forms are stable extensions of the built-in
TypeScript convention. Profile patterns remain available for repository-specific
forms.

**Keep legacy gain and import wording as presentation shorthand.** Rejected
because narration is part of the recommendation contract. It must describe the
actual sign of the score change and the edits a maintainer would need.

## Consequences

An infeasible analysis can legitimately produce no candidate. Users still see
the hard findings and profile baseline, but Strata does not recommend a
restructuring unless the full objective improves.

Capacity values and candidate ordering can change because all configured levels
now contribute consistently. The additional capacity accounting and symbol
admission checks add work to tentative scoring, accepted as the cost of keeping
findings, scores, and recommendations aligned.

Qualified TypeScript test files no longer contribute production relocation
pressure under the built-in test policy. Files using other conventions still
require explicit profile patterns.

Some score-improving symbol destinations are no longer eligible when their only
evidence is an outgoing dependency or when they would collide by declaration
name. Moves toward an actual consumer and consumer-free moves toward a
dependency remain eligible under the existing guardrails.

Saved results remain schema-compatible. Renderers receiving a non-improving
candidate from another producer will label it as a regression and decline it
rather than presenting it as a gain.
