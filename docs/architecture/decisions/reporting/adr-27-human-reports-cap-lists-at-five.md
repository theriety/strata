# ADR-27: Human reports cap each list at five

📌

Human text and Markdown reports show the five worst items of each list and one line counting the rest; `--full` restores the complete lists, and JSON and exit codes always use every finding.

- Status: `Accepted`
- Date: `2026-10-07`

## 🎯 Motivation

Strata's own repository printed 3,224 lines from one `analyze` run. A report that long hides the worst findings and the best moves under thousands of lesser ones, and nobody reads it to the end.

## 🧭 Context

[ADR-16](adr-16-plain-text-reports-and-relocation-endpoints.md) made the default report list every action, essential warning and conditional prerequisite, and kept `--verbose` for added detail. That suited small projects and does not scale to large ones.

## ✅ Decision

- **Five per list.** Human text and the Markdown `report` cap every list at five lines: shared findings, each profile's specific findings, each candidate's proposed moves, advice **Recommended** and advice **Review candidate**, each capped on its own, and the `violations` table. For proposed moves a line is one file move, one mirrored test move or one symbol move, so a narration group of many files is cut like any other list; its `Why:` caption prints once, after the last shown line of that group.
- **Worst first, where there is a worst.** For findings, advice and the `violations` table, the five shown are the worst: violations before warnings, stronger items before weaker, then the existing deterministic order. Proposed moves carry no ranking of their own and are shown in the engine's plan order.
- **Sorted only when something is hidden.** A list of five or fewer keeps its existing order, so a short list reads the same with and without `--full`; ranking applies only when a list is cut.
- **True totals.** Headers keep the real count, for example `Shared findings (456)`.
- **One line for the rest.** When items are hidden, one line follows the list, indented like its items: `… N more — rerun with --full to see all`. Nothing is added when no item is hidden.
- **`--full` restores the old output.** `analyze`, `report` and `violations` take `--full`; with it the text is byte-identical to the output before this decision.
- **Data is never capped.** JSON output, `violations --format json` and the saved result file carry every item. `violations --fail-on` counts every finding, not the five shown.
- **One cap, one helper.** A single constant and a shared helper in the CLI renderer apply the cap to every list.
- The advice disclaimer line stays in every human output.

This partly supersedes [ADR-16](adr-16-plain-text-reports-and-relocation-endpoints.md): the line "The default report includes all actions, essential warnings, and conditional prerequisites" now holds only with `--full`.

## 🔀 Alternatives considered

**A configurable limit.** Rejected: one fixed number keeps reports comparable, and `--full` already covers the need for everything.

**Cap only the largest lists.** Rejected: a rule that differs by list is harder to explain than one number.

## ⚖️ Consequences

- The default report may omit actions a reader would have taken; the "more" line says so and names the flag.
- Scripts that parse the human text for every item must pass `--full` or read JSON.

## Amendment 2026-10-08: tidy text

- Wrapping stays lossless in meaning but no rendered line ends in whitespace; a break consumes the whitespace it falls on, so joining wrapped lines with one space restores the text (refines ADR-16's "lossless wrapping").
- A section with nothing to show takes one line: profiles that all produced zero candidates print one `name — 0 candidates (baseline ...; ...)` line each and skip the explanatory paragraph, and empty Advice prints `  None.` without its explanation. JSON is unchanged.
- **Cycle cuts lead with file-crossing edges.** `EdgeBreak.same_file` (skipped when false; result schema stays 10) marks a cut whose endpoints share a file. Breaks list file-crossing edges first, keeping solver order within each group. A same-file cut is still required but does not separate files, so reports label it "Also cut (same file)". Solver weights and the break set are unchanged; only the JSON `break_suggestions` order may differ.
