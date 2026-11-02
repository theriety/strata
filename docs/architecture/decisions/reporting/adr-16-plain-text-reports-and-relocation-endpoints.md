# ADR-16: Plain-text reports and relocation endpoints

📌

`analyze` defaults to the same plain-text report for every output sink, terminal and Markdown reports carry identical content at each verbosity level, and before/after trees name each moved symbol's source and destination.

- Status: `Accepted`
- Date: `2026-09-08`

## 🎯 Motivation

Strata users inspect findings, compare candidate layouts, and assess relocation advice in terminal and saved Markdown reports. Different content and ordering make those views difficult to compare. Long numerical explanations can obscure the moves themselves. Users also need to see both the source and destination structure without listing an entire unchanged project.

## 🧭 Context

Candidate membership alone does not establish recommendation confidence.

## ✅ Decision

Both human-readable formats present the same information in the same order at each verbosity level. A compact project summary precedes **Structural findings**, **Candidate layouts**, and **Advice**. Reports are static text; Markdown changes styling rather than selecting different content. Reports from saved results do not consult the source repository.

`analyze` defaults to the same plain-text summary on a terminal, in a pipe, and when redirected or written with `--output`. JSON requires explicit `--format json`. Existing explicit formats and the Markdown `report` command remain available. Scripts consuming saved results must request JSON explicitly.

Structural findings contain shared and profile-specific evidence, locations, limits, and suggested cycle cuts. Candidate layouts list every emitted candidate in deterministic order. Both Anchored and Greenfield may return multiple candidates, requesting up to three per profile by default. Candidate numbering remains visible; fewer candidates may survive the diversity, validity, and strict-improvement checks. Each includes its baseline and resulting score, signed improvement, exact moves, changed-folder impacts, and before/after trees. Lower scores are better; scores compare only within the same parameter profile and project. A raw candidate is a proposal, not an instruction to apply every move.

Advice retains the **Recommended** and **Review candidate** groups defined in [ADR-14](../advice/adr-14-profile-consensus.md). Wrapped action lists identify their grain and retain supporting, qualifying, absent, and conflicting profiles, conflicting destinations, and plain-language review reasons. Terms are defined at first use; agreement between profiles is not independent confirmation.

The default report includes all actions, essential warnings, and conditional prerequisites. `--verbose` on `analyze` and `report` adds score-component deltas under each candidate, complete numerical evidence and thresholds under each advice entry, and effective configuration at the end. JSON content and schema are unaffected by verbosity.

### Before and after trees

Each candidate is compared with the saved current layout. Show only affected branches with enough ancestors to locate the changes. Changed filenames end in `*`; a file is changed when it moves or its symbol contents change. Moved symbols appear beneath their actual before file with `[to path/to/destination]` and their actual after file with `[from path/to/source]`. The destination is the final file location after any whole-file relocation; the source is its original location before any relocation. Endpoint paths use the same root-relative and multi-root qualification rules as the report. Whole-file relocations and successful test mirrors are included. Files gaining or losing symbols remain visible even when their path stays the same. A retained empty file is not a deletion.

```text
Before
navigators/
├── navigator.ts *
└── types/
    └── snapshot.ts *
        └── type `AriaTreeOptions` [to navigators/navigator.ts]

After
navigators/
├── navigator.ts *
│   └── type `AriaTreeOptions` [from navigators/types/snapshot.ts]
└── types/
    └── snapshot.ts *

* File moved or its symbol contents changed.
Unchanged branches and symbols omitted.
```

The omission notice distinguishes a partial view from a complete project tree. Excluded or unsupported files are not implied to have disappeared. Candidates without changes state that explicitly. Unchanged placements are never presented as suggested actions.

### Text vocabulary and layout

Every action identifies whether it concerns files, symbols, containers, or visibility. Names are backtick-quoted in prose and tables. Headings use sentence case. Paths are consistently root-relative, with root qualification retained where needed to distinguish multiple roots. Trees use filename labels and directory hierarchy rather than duplicating full paths on every row.

Human-readable lines are deterministic and at most 100 display columns, with lossless wrapping and aligned numerical columns. Tree connectors `├`, `└`, `│`, and `─`, and the `*` change suffix are permitted alongside ASCII and the punctuation `— · § → …`. Input names retain their original characters. Any intentional omission is labeled; displayed candidate counts match the payload. Unsupported future capabilities must be explicitly identified rather than represented as emitted advice.

## 🔀 Alternatives considered

- Different terminal and Markdown detail: rejected because equal verbosity should communicate the same evidence and actions.
- Candidate summaries only: rejected because users want each proposed layout independently inspectable, including its exact moves and impacts.
- Complete project trees: rejected because unchanged branches obscure the affected structure in larger repositories.
- Full numerical detail by default: rejected because numerical evidence is useful on demand while essential reasons must remain immediately visible.
- Switching implicitly to JSON for pipes: rejected because the same command should present the same report regardless of its sink.
- Direction-only symbol annotations: rejected because explicit endpoints let readers follow moves between trees.
- A single combined tree: rejected because paired trees make source and destination placement explicit.

## ⚖️ Consequences

Reports remain inspectable without losing confidence distinctions or machine-readable evidence. Default reports repeat candidate moves in consolidated advice deliberately, but defer detailed numerical tables to verbose output. One shared report representation keeps content parity testable. Presentation regressions cover both formats, both verbosity levels, paired trees, path disambiguation, and wrapping; analysis and saved-result compatibility remain separate invariants.
