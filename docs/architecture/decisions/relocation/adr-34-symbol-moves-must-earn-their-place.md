# ADR-34: Symbol moves must earn their place

📌

A symbol move now passes five gates before the symbol pass accepts it. It must export nothing new. It must gain a share θ of the score it starts from. It must go to the best-scoring admitted destination. That destination must hold an untied majority of the symbol's external pull. Pull counts every edge, so call sites count wherever the IR states them.

- Status: `Accepted`
- Date: `2026-11-24`
- Resolves: F3–F9 of the mdc false-move findings (SC-2, SC-3), and the namesake-symbol Review candidates that ADR-32 surfaced

## 🎯 Motivation

The symbol pass (`crates/engine/src/analyze/relocation/symbol.rs`) took the first nominated destination that passed the vetoes and improved the objective by more than float dust (`SYMBOL_MIN_IMPROVEMENT`). Destinations were ranked by summed priced pull, and ties went to the lower file id. On mdc this produced moves a reader rejects at once:

- **F3–F7.** An exported entry point was moved away from the file-private helpers it calls (`inferMatches` and the `annotation.ts` readers). Each such move forces those helpers to become exported and imported across files.
- **F8.** `hasMetadata` is used by `inline.ts` (3), `block.ts` (2), `inline/reference.ts` (1) and `document.ts`. The file-cycle veto refused the majority consumers and the depth rule refused `inline/reference.ts`. So it landed in `document.ts`, a minority consumer, for a relative gain of about 2.1e-4.
- **F9.** `appendToken` has 3 call sites in class `InlineScanner` (`scanner.ts`) and 1 in class `BracketResolver` (`brackets.ts`). The TypeScript adapter states one edge per calling declaration, so both classes pulled 1.0. The tie went to the lower file id, `brackets.ts`.

## ✅ Decision

The gates live in the symbol pass. They run in this order for each nominated destination (`SymbolPass::trial`), and the acceptance test runs once over the best trial (`SymbolPass::try_relocate`).

1. **No new exports** (`symbol/exports.rs`). A declaration is *private* when it is declared at file scope, is not public API, is not a re-export, and every pass-start consumer sits in its own file. A move is refused when the symbol depends on a private declaration that stays behind, or when the symbol is private itself and a declaration staying behind depends on it. Either way a private declaration would become exported and newly imported. Collision folds (ADR-21) move a whole file, so nothing stays behind and the guard does not apply to them.
2. **Relative gain threshold θ.** A move must gain more than `θ × |score before the move|`, and never less than the float-dust floor. θ is the profile key `relocation.symbol-gain-threshold`. It must be finite and at least `0`, and `0` keeps only the floor. The default is `0.001` in both profiles, calibrated on the eval corpus (see Calibration). Folds keep only the floor.
3. **Best admitted destination.** Every nominated destination (still at most `SYMBOL_TARGETS`) runs through every veto and is priced. The lowest total wins. If two destinations tie on the best total, the symbol stays home. Nomination ranks equal pull by file path, not by file id, so no outcome depends on container numbering.
4. **Minority-consumer rule** (`symbol/pull.rs`). A destination must hold at least half of the symbol's external pull (`SYMBOL_MAJORITY_SHARE = 0.5`, fixed). No other file may pull as hard. A file holding less is a minority consumer, and a tied file has no better claim than the current home, so both keep the symbol home. External pull is the same two-way priced pull that nominates, taken from base-placed residents outside the symbol's file.
5. **Call-site-aware pull.** Pull sums every edge, so parallel edges between two declarations add up. An adapter that states one edge per call site therefore gets call-site pull. The IR carries no multiplicity field, and the TypeScript adapter states one edge per calling declaration (`emit_calls` deduplicates targets). For that adapter the tie rule of gate 4 is the fallback: the brief allows changing neither adapters nor the IR.

## 📏 Calibration

Measured on the 10 eval targets with seed 42 and k = 3. θ was swept over {0, 1e-4, 3e-4, 1e-3, 3e-3, 1e-2}, then 3e-2 and 0.1, with the other coefficients at their pre-calibration defaults (ε = ω = `0.05`). The joint check repeated the θ grid at ε = ω ∈ {0.001, 0.0025, 0.005, 0.0125}.

| θ | Every case (ε = ω = 0.05) | Every case (ε = ω = 0.0125) | Recommended | Review | Symbol moves in any candidate |
| --- | --- | --- | --- | --- | --- |
| `0` | pass | pass | 0 | 23 | 1 |
| `0.0001` | pass | pass | 0 | 23 | 1 |
| `0.0003` | pass | pass | 0 | 23 | 1 |
| `0.001` (chosen) | pass | pass | 0 | 23 | 1 |
| `0.003` | pass | pass | 0 | 23 | 1 |
| `0.01` | pass | pass | 0 | 23 | 1 |
| `0.03` | pass | — | 0 | 22 | 0 |
| `0.1` | pass | — | 0 | 22 | 0 |

- **Band.** The `companion-folders` F3–F9 pins and every other pin hold for every θ from `0` to `0.1` (measured to `0.01` at the calibrated ε), so the corpus does not bound θ from either side. No pinned move is lost anywhere in the band.
- The only symbol move that survives in any candidate is `discount_floor` from `billing/pricing.py` to `billing/ledger.py` (tearing, greenfield). It gains `0.0176` of its score, appears as an unpinned Review candidate, and is lost from θ = `0.03`.
- θ matters only when ε is too small. At ε = `0.001`, F6 (`readInlineSelector`, relative gain `0.0071` when ε = 0) fails at θ ≤ `0.0001` and holds at θ ≥ `0.0003`.
- **Rule.** Keep θ inside the green band, at least five times below the smallest relative gain of a move the corpus keeps (`0.0176 / 5 ≈ 0.0035`), and large enough to filter float-scale gains. `0.001` meets all three, 17 times below that gain, so the default stays at `0.001`.

## 🔀 Alternatives considered

- **An absolute gain floor ε.** Rejected: the mean imbalance term (ADR-32) and the normalized cut rescale every term with the tree, so a fixed ε is too strict on small repositories and too lax on large ones.
- **Majority share as a profile key.** Rejected for now: no case needs another value, and the rule is structural rather than a tuning knob.
- **Ties broken toward the lower path, as nomination does.** Rejected for the final pick: a tie between two destinations means the evidence does not decide, and every other tie in the pass already resolves to staying.
- **A namesake heuristic** (keep `parse` in `parse.rs`). Rejected: ADR-6 forbids path and name heuristics, and the α·naming term scores token overlap between co-resident declarations, not between a declaration and its file name. So it does not cover this case either. The gates remove the namesake moves that lack majority support (`parse`, `scale` and `unit` in workspace-move-rust). They keep a move into a symbol's sole priced consumer (see Consequences).
- **Call-site multiplicity in the IR.** Deferred: it needs an adapter and IR change (multiplicity or weight on `Edge`), which is outside this decision's scope.

## ⚖️ Consequences

- F3–F9 no longer reach the advice. The `companion-folders` eval case is green apart from the C1 TODO owned by the cycle work.
- Under the 0.5 share with the tie rule, at most one file can pass gate 4. Gate 3 therefore matters for its stable ordering, for its tie rule, and for any later change to the share.
- The no-new-exports guard reads declared visibility only where an adapter states it (Rust). TypeScript and Python declare every node at file scope, so for them *private* means "used only in its own file", whether or not the source exports it.
- A move into `x.ts` from `x/*.ts` needs no extra margin: ADR-33's `entry-inflow` term already prices it.
- Since ADR-31, the depth rule measures a companion entry at its folder's depth, so a declaration in a parent folder cannot move down into a companion folder. That outcome is intended. A move into a deeper folder was never allowed.
- `Shape` still moves into `rectangle.ts` (nested-ts, dup-geo-py): `Rectangle` is its only priced consumer, because the barrel's re-export edge weighs `0`. `describe` still moves into `app.ts` (nested-ts): a dynamic import, at confidence 0.5, is its only priced consumer and holds 62.5% of its pull. All three moves pass every gate. Only the file name argues against them, and ADR-6 rules that evidence out.
- Score breakdowns and the result schema do not change. The effective-configuration line of verbose output shows θ.
