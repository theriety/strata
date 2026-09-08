> **Status:** Superseded
>
> **Superseded by:** [ADR-0016 — Plain-text reports and relocation endpoints](../0016-plain-text-reports-and-relocation-endpoints.md)
>
> **What changed:** Partial: plain text becomes the default for every output sink, and symbol annotations identify relocation endpoints.

# ADR-0015: Consistent reports and change trees

- Status: `Accepted`
- Date: `2026-09-07`

## Context

Strata users inspect findings, compare candidate layouts, and assess relocation advice in terminal and saved Markdown reports. Different content and ordering make those views difficult to compare. Candidate membership alone does not establish recommendation confidence, and long numerical explanations can obscure the moves themselves. Users also need to see both the source and destination structure without listing an entire unchanged project.

## Decision

Both human-readable formats present the same information in the same order at each verbosity level. A compact project summary precedes **Structural findings**, **Candidate layouts**, and **Advice**. Reports are static text; Markdown changes styling rather than selecting different content. Reports from saved results do not consult the source repository.

Structural findings contain shared and profile-specific evidence, locations, limits, and suggested cycle cuts. Candidate layouts list every emitted candidate in deterministic order. Each includes its baseline and resulting score, signed improvement, exact moves, changed-folder impacts, and before/after trees. Lower scores are better; scores compare only within the same parameter profile and project. A raw candidate is a proposal, not an instruction to apply every move.

Advice retains the **Recommended** and **Review candidate** groups defined in [ADR-0014](0014-profile-consensus.md). Wrapped action lists identify their grain and retain supporting, qualifying, absent, and conflicting profiles, conflicting destinations, and plain-language review reasons. Terms are defined at first use; agreement between profiles is not independent confirmation.

The default report includes all actions, essential warnings, and conditional prerequisites. `--verbose` on `analyze` and `report` adds score-component deltas under each candidate, complete numerical evidence and thresholds under each advice entry, and effective configuration at the end. JSON content and schema are unaffected by verbosity.

### Before and after trees

Each candidate is compared with the saved current layout. Show only affected branches with enough ancestors to locate the changes. Changed filenames end in `*`; a file is changed when it moves or its symbol contents change. Moved symbols appear beneath their actual before file with `[moves out]` and their actual after file with `[moved in]`. Whole-file relocations and successful test mirrors are included. Files gaining or losing symbols remain visible even when their path stays the same. A retained empty file is not a deletion.

```text
Before
navigators/
├── navigator.ts *
└── types/
    └── snapshot.ts *
        └── type `AriaTreeOptions` [moves out]

After
navigators/
├── navigator.ts *
│   └── type `AriaTreeOptions` [moved in]
└── types/
    └── snapshot.ts *

* File moved or its symbol contents changed.
Unchanged branches and symbols omitted.
```

The omission notice distinguishes a partial view from a complete project tree. Excluded or unsupported files are not implied to have disappeared. Candidates without changes state that explicitly. Unchanged placements are never presented as suggested actions.

### Text vocabulary and layout

Every action identifies whether it concerns files, symbols, containers, or visibility. Names are backtick-quoted in prose and tables. Headings use sentence case. Paths are consistently root-relative, with root qualification retained where needed to distinguish multiple roots. Trees use filename labels and directory hierarchy rather than duplicating full paths on every row.

Human-readable lines are deterministic and at most 100 display columns, with lossless wrapping and aligned numerical columns. Tree connectors `├`, `└`, `│`, and `─`, and the `*` change suffix are permitted alongside ASCII and the punctuation `— · § → …`. Input names retain their original characters. Any intentional omission is labeled; displayed candidate counts match the payload. Unsupported future capabilities must be explicitly identified rather than represented as emitted advice.

## Alternatives considered

- Different terminal and Markdown detail: rejected because equal verbosity should communicate the same evidence and actions.
- Candidate summaries only: rejected because users want each proposed layout independently inspectable, including its exact moves and impacts.
- Complete project trees: rejected because unchanged branches obscure the affected structure in larger repositories.
- Full numerical detail by default: rejected because numerical evidence is useful on demand while essential reasons must remain immediately visible.
- A single combined tree: rejected because paired trees make source and destination placement explicit.

## Consequences

Reports remain inspectable without losing confidence distinctions or machine-readable evidence. Default reports repeat candidate moves in consolidated advice deliberately, but defer detailed numerical tables to verbose output. One shared report representation keeps content parity testable. Presentation regressions cover both formats, both verbosity levels, paired trees, path disambiguation, and wrapping; analysis and saved-result compatibility remain separate invariants.
