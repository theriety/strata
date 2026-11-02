# ADR-23: The symbol pass is a thin coordinator over single-job parts

📌

The symbol polish pass is split into a ledger, a pricing part, an admission part and a visibility tracker, with `SymbolPass` keeping only the steps that must consult several of them in one fixed order; every part lives in its own file under `analyze/relocation/symbol/`.

- Status: `Accepted`
- Date: `2026-10-01`

## 🎯 Motivation

The symbol pass carries the most rules in the relocation engine: no empty shells, one move per symbol, the reach guards, the package wall, the test-polarity pin, collision folds. They all lived in one 1100-line file and one struct with about thirty fields. A reader who wanted to know why a symbol was refused had to hold the placement ledger, the pricing maps and the veto sets in mind at once, and a change to one rule could silently touch state another rule depended on. Strata flagged the file as over its own capacity limit, and the project holds itself to that limit.

## 🧭 Context

The pass is deterministic. Symbols sweep in ascending id order, destinations rank by summed priced pull with ties toward the lower file id, and floating-point sums run in a fixed order. Golden outputs and parity tests pin the result, so any restructuring must leave relocations, their order, the objective values and the narration byte-identical. [ADR-21](adr-21-a-colliding-file-move-folds-into-symbol-moves.md) and [ADR-22](adr-22-test-symbols-are-pinned-by-polarity.md) added vetoes that run on both ordinary moves and folds, so those paths have to keep sharing one admission check.

## ✅ Decision

`SymbolPass` becomes a thin coordinator. Each part below owns one kind of state and the methods that read only that state; each is a separate file under `analyze/relocation/symbol/`.

- **Ledger** (`ledger.rs`) owns the placement overlay, the running-best score, per-file production SLOC, occupancy and native-resident counts, the set of moved nodes, and the accepted relocations. It guards one move per symbol (FIX12-C) and the native count behind "no empty shells", which arrivals never raise (FIX12-A).
- **Pricing** (`pricing.rs`) owns the both-direction priced incidence, the file vertices and the objective. It nominates destinations (FIX04, FIX12-B), scores a placement, and builds the file crossing graph the cycle floor reads.
- **Admission** (`admission.rs`) owns the relocation policy and every structural veto: pinned sources and destinations, the package wall (ADR-17), namespace, depth, name collision, the test-zone boundary, type-only files, and the three guards (the folder reach guard (FIX13), the pass-start dependency guard and the consumer-branch guard), which read the immutable pass-start evidence held in `reach.rs` and `guards.rs`. Ordinary relocations and collision folds (ADR-21) both pass through it, and the polarity pin (ADR-22) is part of the policy it holds.
- **Visibility tracker** (`visibility.rs`) owns the working copy of the node table and the finding-count baseline that no relocation may raise.
- **Outcome and entry points** (`outcome.rs`, `polish.rs`, `narrate.rs`) hold the result type and the `PipelineSolver` functions that build a pass, run it, score the result and narrate it. `inputs.rs` holds the read-only inputs and the policy handed in.

The coordinator keeps the cycle baseline and the methods that consult several parts in one order: construction, the fold (`fold.rs`) and the sweep, the veto ladder for one symbol, and placing and reverting a tentative move. The ladder stays one function because its order is the rule: splitting it would scatter checks that must run in sequence.

The split is a pure move. Method bodies are unchanged apart from field paths, and no loop, sum or tie-break changed order. Where a method read two parts, one of them is passed in rather than copied.

## 🔀 Alternatives considered

**Move the existing impls into child files and keep one struct.** Rejected because the files would shrink while the thirty-field struct and its tangled state would not.

**Waive the capacity limit for this file.** Rejected because the file is the main place the veto rules live, and a waiver would leave it the hardest file to review.

**Make each veto its own type behind a trait.** Rejected because the vetoes already run in a fixed order with shared pass-start facts, and a trait boundary would add indirection without isolating any state.

## ⚖️ Consequences

Each part can be read and tested on its own terms, and a new veto belongs in one obvious place. The coordinator reads as the order of operations. Outputs, goldens and parity are unchanged.

The cost is a few more files, `&Ledger` threaded into the admission and crossing-graph calls, and a handful of items visible to the whole `symbol` module (`pub(super)`) so siblings and the tests can reach them. Nothing is wider than before outside `symbol`. Five kinds of test read changed path only, with no change to any assertion or setup value: `pass.best` is now `pass.ledger.best`, `pass.relocations` is `pass.ledger.relocations`, `pass.overlay` is `pass.ledger.overlay`, `pass.incident` is `pass.pricing.incident`, and `pass.vis_base` is `pass.visibility.vis_base`.
