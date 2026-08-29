# Architecture

[strata — Architecture](overview.md) describes the current structure: the
adapter → IR → engine pipeline, crate layering, snapshot contract, and
decomposition phases. [Deviations](deviations.md) records where implementation
departed from spec.

## Decisions

| Document | Title | Status |
| --- | --- | --- |
| [ADR-0001](decisions/0001-recommendations-only.md) | Recommendations only — strata never delivers or executes move scripts | Accepted |
| [ADR-0002](decisions/0002-report-format-action-list.md) | Report format — suggested action list vocabulary for all suggestion faces | Accepted |
| [ADR-0004](decisions/0004-same-file-references-are-dependencies.md) | A same-file reference is a dependency edge | Accepted |
| [ADR-0005](decisions/0005-pass-start-dependencies-govern-symbol-moves.md) | Pass-start dependencies govern symbol moves | Accepted |
| [ADR-0006](decisions/0006-symbol-destination-guardrails.md) | Symbol destinations obey pass-start guardrails | Accepted |
| [ADR-0007](decisions/0007-relocations-preserve-render-namespace-identity.md) | Relocations preserve render namespace identity | Accepted |
