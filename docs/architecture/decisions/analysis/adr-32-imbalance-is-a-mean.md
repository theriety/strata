# ADR-32: Size imbalance is a mean over containers, not a sum

📌

The objective's size-imbalance term averages the squared coefficient of variation of child sizes over every container with at least two children. It no longer sums it over all containers. λ keeps its default.

- Status: `Accepted`
- Date: `2026-11-24`
- Resolves: the imbalance-dominance residual recorded under D-44 (`docs/architecture/deviations.md`)

## 🎯 Motivation

`J(T)` adds `λ·imbalance(T)` to the normalized cut, which lies in `0..=1` (D-37). Imbalance, however, was a sum over every container, so it grew with the number of folders. On a 154-file TypeScript repository (mdc), raw imbalance was about 21.9 against a cut of about 0.4. The leading anchored candidate gained −0.2737 from imbalance and lost only +0.0002 in cut. Unlace therefore proposed moves that only evened out folder sizes and pulled files away from the code they depend on. One of these, hoisting `tree/validation/shape.ts` out of its companion folder, was the run's only `Recommended` move. ADR-3 and ADR-4 had already noted that imbalance dominates, and D-44 left that as a residual.

## ✅ Decision

1. `imbalance(T) = mean over { c : |children(c)| ≥ 2 } of CV²(child sizes of c)`, and `0` when no container has two or more children. A container whose children are all empty contributes `0` to the mean.
2. λ keeps its default (D-49/D-50: defaults are law). The term's scale changes instead, so that it is again comparable to the other terms, as the module doc of `crates/core/src/score.rs` already claimed.
3. Goldens and eval pins move with the new scores. Each re-blessed diff is reviewed: the expected change is in scores and in which moves win, not in the shape of findings.

## 🔀 Alternatives considered

- **Lower λ and keep the sum.** Rejected: the sum still grows with repository size, so any fixed λ is wrong for either small or large repositories.
- **Normalize by the total container count.** Rejected: single-child containers have nothing to balance, and counting them dilutes the term, more in deep trees than in flat ones.
- **Remove the term.** Rejected: with λ = 0, the greenfield profile hoisted companion-folder code into namesake files (probe in the work stream). Size balance still has to counter that pull.

## ⚖️ Consequences

- Imbalance now ranges over `0..=CV²max` regardless of repository size, and cohesion terms decide moves again.
- Every reported score and imbalance delta changes. Result JSON keeps its schema, since only values change.
- On its own, this change does not stop companion hoisting or minority-caller moves. ADR-31, ADR-33 and ADR-34 address those.
- On small fixtures the summed term had also been hiding greenfield moves that pull a symbol out of the file named after it: `parse` out of `parse.rs` (workspace-move-rust), `Shape` out of `shape.ts` (nested-ts). These moves now surface as Review candidates. The symbol-relocation gates of ADR-34 have to remove them. Changing this term alone does not.
