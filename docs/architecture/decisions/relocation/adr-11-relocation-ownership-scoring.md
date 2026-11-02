# ADR-11: Relocation ownership and dependency-only scoring

📌

Each profile charges a fixed `dependency-only` cost for moving a declaration toward a destination that holds only its dependencies and none of its consumers, and a profile-independent guard stops a shared declaration from being buried in one consumer branch.

- Status: `Accepted`
- Date: `2026-08-30`

## 🎯 Motivation

A declaration can depend on a helper in one file while its consumers remain elsewhere. Treating every incident dependency as equivalent destination evidence can move the declaration beside something it uses even though nothing in that destination uses it.

When a declaration serves consumers in sibling branches, an unrelated existing dependency between those branches could authorize burying the shared declaration inside either branch.

## 🧭 Context

Symbol relocation needs to distinguish ownership from dependency proximity. Folder-level dependency reach is insufficient ownership evidence.

These decisions must remain deterministic across a relocation pass. Scoring, admission, candidate ordering, reported deltas, and saved results must use the same immutable analysis-start evidence and the selected parameter profile's effective coefficients.

## ✅ Decision

Each parameter profile has a `dependency-only` objective coefficient, defaulting to `0.05`. The objective charges that fixed amount once for each relocated production declaration whose destination contained, at analysis start:

- at least one target of the declaration's outgoing structural dependency edges; and
- no incoming consumer of the declaration.

The classification uses all structural IR edge kinds, ignores self-loops, and uses immutable analysis-start files. The current layout therefore has zero dependency-only pressure. A coefficient of `0.0` disables the charge. The same term governs tentative symbol admission, candidate ordering, total scores, score breakdowns, gains, and narrated symbol-move deltas.

Consumer ownership is a profile-independent hard guard. Incoming consumers are resolved to their analysis-start physical folders. When those consumers occupy multiple child-folder branches beneath their lowest common ancestor, the declaration may not move into only one consumer branch. Existing folder reach does not grant an exemption. A move remains admissible when the declaration has a single consumer, all consumers occupy one folder, the destination is a neutral shared branch, or the destination is the consumers' common ancestor.

Analysis results use schema version 5. Score breakdowns expose the dependency-only term as `dependencyOnly` in JSON and `dependency-only` in configuration and human output. Readers reject unsupported earlier schema versions rather than inferring the missing term.

## 🔀 Alternatives considered

**Hard-veto every move toward a dependency-only destination.** Rejected because dependency proximity can be useful evidence. A profile-adjustable objective term preserves that evidence while requiring enough benefit to justify separating a declaration from its consumers.

**Use only folder-level dependency reach to protect shared declarations.** Rejected because an unrelated edge between folders can manufacture permission for a move whose actual consumers still span sibling branches.

**Recompute ownership and destination evidence after each accepted move.** Rejected because earlier moves could manufacture permission or pressure for later moves. One pass must be governed by one immutable evidence base.

**Normalize the charge by repository size or dependency count.** Rejected because the policy is intended to express a clear per-declaration cost. A fixed coefficient is reproducible and directly configurable.

**Apply the charge only in final candidate scoring.** Rejected because admission, candidate ordering, reported deltas, and totals would then disagree about the value of the same relocation.

## ⚖️ Consequences

Weak dependency-only moves must overcome an explicit profile-local cost, while a profile can restore the uncharged behavior by setting `dependency-only = 0.0`. Moves toward actual consumers remain eligible and do not incur the term.

Shared declarations cannot be buried inside one sibling consumer branch, even when unrelated pass-start dependencies already connect the branches. Moving toward a common or neutral owner remains possible.

Candidate scores, gains, and symbol deltas can change because they now include the same additional term. Saved-result consumers must migrate to schema version 5 and read `dependencyOnly`; configuration and report consumers must recognize `dependency-only`.
