# ADR-0002: Report format — suggested action list

- **Status:** Accepted
- **Date:** 2026-08-23
- Accepted by owner ruling after design rounds 5–10; see Context.

## Context

Strata's reports propose codebase structure changes as a static text dump. Ten
design rounds, two independent cold-reader comprehension gates, and two
independent usefulness critiques established that earlier report faces failed
their reader twice over: field vocabulary was unexplained (leaf, weight, score
terms, gain), and the pages carried content that demanded no action — keep
rows, marker columns whose every remaining value said the same thing,
self-description of the report, internal QA provenance.

The owner ruled the format through successive rounds; this ADR records the
accepted vocabulary so renderer work (DSG04) and future changes have one
durable authority.

## Decision

Strata's suggestion reports are **suggested action lists**:

1. **No action = no suggestion.** Unchanged placements are never listed.
   Files that stay are not suggestions; container blast-radius deltas carry
   the "what stays" information instead.
2. **No marker column, no removal entries.** Change tables are exactly
   `leaf | before place | after place`. Strata does not propose deletions;
   if a candidate implies something disappears, it renders as relocation or
   is omitted.
3. **Every suggestion states its grain** — whole files, symbols within one
   file, packages above files, or visibility-only — explicitly, per
   suggestion.
4. **Names are quoted.** File, folder, and symbol names appear in backticks
   in prose and tables: Move `logger` into `io/`.
5. **Sentence case headers.** Full capitals are banned outside caption
   anchors so names remain readable.
6. **Deterministic fixed-width text, ≤100 columns.** The only permitted
   non-ASCII glyphs are em dash, middle dot, section mark, rightwards arrow,
   horizontal ellipsis (— · § → …); everything else is ASCII. Truncation is
   honest (`+N more` markers); claimed candidate count = listed = payload.
7. **Scores compare only within one mode of one project**, stated once beside
   the candidate list. Cross-report benchmark context is rejected — it
   contradicts this rule.
8. **Field definitions ship at first use**, in plain language (leaf, weight,
   score terms, modes, gain).
9. **Capabilities the engine cannot yet emit render as an explicitly labeled
   future ledger** (symbol-grain rows until FIX08 lands).
10. Output remains a static text dump; HTML is design-medium only
    (reaffirms ADR-0001).

## Consequences

- The renderer port (DSG04) implements this vocabulary; goldens are re-blessed
  deliberately under the R3 flow.
- Excluding keeps means reports no longer enumerate unchanged files; the
  blast-radius delta block carries that information instead (accepted trade).
- Per-suggestion score attribution and per-move broken-import counts land with
  FIX08 (owner ruling 2026-08-23); until then any such rows render only as
  future-ledger entries.
