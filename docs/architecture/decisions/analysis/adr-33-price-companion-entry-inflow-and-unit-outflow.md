# ADR-33: Price companion entry inflow and unit outflow

📌

The objective charges a fixed amount for each declaration moved from a companion folder `x/` into its entry file `x.ts` (entry inflow, ε), and for each file or declaration that leaves the unit `x/ ∪ x.ts` (unit outflow, ω). Both are priced terms, not vetoes. ε is calibrated on the eval corpus at `0.0125`; ω has no isolating eval case and tracks ε.

- Status: `Accepted`
- Date: `2026-11-24`
- Resolves: the symbol-grain companion false moves F4–F7 found by the mdc run (`advice-false-moves`)

## 🎯 Motivation

The mdc parser follows the companion-folder convention: `lexer/block.ts` is the entry of `lexer/block/`, and the folder holds the parts only the entry and its siblings use. Unlace v1.0.0 proposed moves that broke this structure. It hoisted `tree/validation/shape.ts` out of its folder (F1) and `lexer/block/mdcx-body.ts` toward `lexer/` (F2). It moved `inferMatches` away from its private helpers (F3). It also pulled `readMdcxBlock` up from `lexer/block/mdcx.ts` into `lexer/block.ts` (F4), and `buildAnnotation`, `readInlineSelector` and `readInlineSuffix` up from `parser/inline/annotation.ts` into `parser/inline.ts` (F5–F7).

ADR-31 made `x.ts` count as the entry of `x/` for structure, which keeps whole files in their folder. It does not stop a declaration moving up into the entry, because the entry already calls it and the cut term rewards the move. After ADR-31, the greenfield profile of the `companion-folders` eval case still moved `readMdcxBlock`, `readInlineSelector` and `readInlineSuffix` into their entries. A file's private implementation was split from its only public symbol, and the folder was left with a stranded helper file.

## 🧭 Context

- A **unit** is a folder `x/` that has a folder-companion fact (`FileFact.folder_companion`) together with its entry `x.ts`. Only an adapter can tell that `x.ts` is an entry. The engine reads the fact through `unlace_ir::laminar::Companions` and gets a file's structural directory from `effective_directory`. For `x.ts`, that directory is `x`.
- `companion-separation` (κ, ADR-12) already prices a structural preference as a fixed charge per separated item. It is counted at two grains: declarations placed by the symbol pass and files placed by the file pass.
- ADR-6 forbids path heuristics, so the engine cannot infer an entry from a file name.

## ✅ Decision

1. **Entry inflow.** `entry_inflow(T)` counts production declarations whose pass-start file lies inside a unit's folder, but is not that unit's entry, and whose candidate file is the unit's entry `x.ts`. A declaration moved from `x.ts` into `x/a.ts` is not inflow.
2. **Unit outflow.** `unit_outflow(T)` counts the items whose pass-start effective directory lies inside a unit (`x` or below, which includes `x.ts`) and whose candidate location lies outside it.
   - A declaration's location is its candidate file's pass-start effective directory.
   - A file's location is the folder key it sits under in the candidate tree, compared with the folder key the unit's entry sits under at pass start.
   - A file that leaves counts once, however many declarations it holds. Its declarations do not count again, because they stay in their file.
3. Both counts use the grain `companion_separations` uses. A move inside `x/`, or a move into the unit, counts toward neither. A folder without a companion fact forms no unit.
4. `J(T) = … + κ·companion_separation + ε·entry_inflow + ω·unit_outflow`. The configuration keys are `objective.entry-inflow` and `objective.unit-outflow`. Validation rejects negative and non-finite values and reports the key path.
5. **Calibrated defaults.** Both coefficients default to `0.0125` in the anchored and greenfield profiles. The other coefficients keep their defaults (D3).
   - **Rule for ε.** Take the smallest value on the grid {0, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1} that clears `companion-folders` while every other eval pin stays green, for every θ on the ADR-34 grid. Multiply it by five as a safety margin. That value is `0.0025`, so ε = `0.0125`.
   - Why every θ: at the default θ = `0.001` the smallest clearing value is `0.001`, but θ is a profile key. The binding move, `readInlineSelector` into `parser/inline.ts` (F6, greenfield), gains `0.001034` on a score of `0.1454` when ε = 0. It stays home only when ε > `0.001034 − θ × 0.1454`, so at θ = 0 it needs ε > `0.001034`, and `0.001` fails.
   - **ω is uncalibrated.** No eval case isolates it: with ε = 0, every ω on the grid fails exactly as ω = 0 does, and with ω = 0 every ε ≥ `0.001` passes. ADR-31 already holds F1 and F2, the unit-outflow moves. ω therefore keeps ω = ε until a case where ω alone decides the outcome exists.
6. The result breakdown gains `entryInflow` and `unitOutflow`. They are additive and default to zero on read, so the result schema version does not change.

## 📏 Calibration

Measured on the 10 eval targets with seed 42 and k = 3, sweeping one coefficient at a time from the pre-calibration defaults (ε = ω = `0.05`, θ = `0.001`). Every other case passes at every value below. No case produced a Recommended move at any value, so the Recommended count does not discriminate. The Review count is the sum over all cases.

| Value | ε swept (ω = 0.05) | ω swept (ε = 0.05) | ε = ω, θ = 0.001 | ε = ω, θ = 0 | Review (ε swept) |
| --- | --- | --- | --- | --- | --- |
| `0` | fail F6 | pass | fail F6 | — | 24 |
| `0.0001`, `0.00025`, `0.0005`, `0.00075` | fail F6 | — | — | — | 24 |
| `0.001` | pass | pass | pass | fail F6 | 23 |
| `0.0025` | pass | pass | pass | pass | 23 |
| `0.005` | pass | pass | pass | pass | 23 |
| `0.01` | pass | pass | pass | — | 23 |
| `0.0125` (chosen) | — | — | pass | pass | 23 |
| `0.025`, `0.05`, `0.1` | pass | pass | pass | — | 23 |

F6 is `preserve_symbol_home(readInlineSelector)#Greenfield`; at ε = 0.001 it also fails at θ = `0.0001` and passes at θ ≥ `0.0003`. At ε = ω = `0.0125`, every case passes for every θ on the ADR-34 grid. "—" marks a cell not measured.

- With ε = 0, every ω on the grid fails F6, as ω = 0 does. ω isolates no outcome.
- Chosen: ε = ω = `0.0125` (`0.0025 × 5`). ε by the rule in decision 5. ω is uncalibrated and tracks ε.

## 🔀 Alternatives considered

- **Hard guard (a symbol-admission veto).** Rejected by D2: the companion effect is a preference that strong evidence can overcome. A veto would also hide the trade from the score breakdown.
- **Inflow only.** Rejected by E2: pricing only the upward pull lets the optimizer move the same code out of the unit sideways or to the parent at no cost. Both directions need a price.
- **Scope the terms to advice qualification only.** Rejected by E1: if the optimizer ranks candidates without the terms, its best candidate is still the torn layout, and the advice tier would only hide it.

## ⚖️ Consequences

- With the ADR-34 gates in place, ε decides one `companion-folders` pin: `readInlineSelector` (F6, greenfield). Below the bound above it moves into `parser/inline.ts`, paid by the imbalance term (`−0.0009`) and the cut (`−0.0001`), and it reaches the advice as a Review candidate. ω alone changes nothing in this case, because ADR-31 already keeps F1 and F2 in place.
- Repositories without folder-companion facts score exactly as before. Their unit facts are empty, and both terms are zero.
- **Index asymmetry.** A folder whose entry is `x/index.ts` has no folder-companion fact, so it forms no unit, and declarations moving into `index.ts` are not priced. The engine cannot tell an index entry from any other file without the path heuristic ADR-6 forbids. Treating index entries as units needs an adapter fact, which is an open question.
- Scores in repositories with companions rise by `0.0125` for each item that crosses a unit boundary. Their goldens and pins move only where a candidate makes such a move.
