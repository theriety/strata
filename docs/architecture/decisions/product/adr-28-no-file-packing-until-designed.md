# ADR-28: No file packing until it is designed

📌

Strata has no file-packing phase: the unwired `pack` module was deleted, and any future packing starts from a fresh design rather than from the old code.

- Status: `Accepted`
- Date: `2026-10-07`

## 🎯 Motivation

Strata's self-analysis flagged `pack` as production code that only tests used. The engine and the CLI have never called it since commit `b161fdd`, yet the architecture overview, the deviations log and several comments described file packing as a phase of the pipeline. A reader could not tell what the tool really did.

## 🧭 Context

`pack` was written as phase 7: cohesion-first Kruskal agglomeration of SCC atoms under the production-SLOC cap, with conditional splits for oversized cyclic groups, about 900 lines in `crates/core/src/pack.rs` and its submodules. The search that shipped works on whole files and folders, and the file-split estimate in `conditional_splits` is a plain ceiling division. Nothing consumed the packing output, so its tests only proved the module agreed with itself.

## ✅ Decision

- **Delete `pack`.** The module, its submodules and its tests are removed from `strata-core`.
- **Docs say what exists.** The overview marks phase 7 as not built, the deviations log records the removal against each entry that mentioned `pack`, and no comment claims that packing exists.
- **Git history keeps the code.** Commit `b161fdd` holds the last version for reference.
- **A future design starts fresh.** Packing returns only through a new decision that names its caller, its output and its tests against real analysis results. It would keep the recommendation-only shape of [ADR-1](adr-1-recommendations-only.md): it would propose splits, never perform them.

## 🔀 Alternatives considered

**Keep `pack` as an unused library module.** Rejected: unused code keeps drifting from the rest of the system, and its tests give false confidence.

**Wire `pack` into the search now.** Rejected: that changes analysis results and goldens, and it is a product change, not a cleanup.

## ⚖️ Consequences

- Reports and goldens are unchanged, because no output ever depended on `pack`.
- File and symbol granularity of splits stays as it is today: `conditional_splits` remains a ceiling estimate.
- Reviving packing costs a new design, not a revert.
