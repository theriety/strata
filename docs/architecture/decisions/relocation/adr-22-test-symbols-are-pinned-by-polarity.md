# ADR-22: Test symbols are pinned by polarity

📌

Each profile's relocation policy can pin files and symbols in place without removing them from analysis; detected test files and symbols with detected test polarity are pinned, and an accepted source move carries its exact test mirrors as best-effort followers.

- Status: `Accepted`
- Date: `2026-09-25`

## 🎯 Motivation

Optimizing test files and test-support declarations independently can produce recommendations that break the source/test layout readers expect. Pinning only symbols that live in detected test files is not enough: Rust `#[cfg(test)]` functions inside production files could be proposed as independent symbol moves. Matching a test to production code by basename is also ambiguous whenever separate modules use a common filename.

## 🧭 Context

Test files and test-support declarations participate in useful dependency, scoring, capacity, and cycle evidence. Strata detects test files, and it detects each symbol's polarity; test symbols have polarity `TestCase` or `TestSupport`, and they can live inside production files.

A source relocation can have one or more test counterparts. Those tests should follow the source when the relationship is exact and the resulting placement is valid, without making the source recommendation depend on every follower succeeding.

## ✅ Decision

Each parameter profile owns a relocation policy. Repo-relative glob patterns can pin files against independent file moves and can prevent symbols from leaving or entering matching files. Pinned participants remain in the graph and continue to contribute to dependencies, scoring, capacity, cycles, and findings. If one file in a strongly connected file component is pinned, the whole component is pinned.

Detected test files are pinned by default. With `pin-detected-test-symbols = true`, a symbol whose detected polarity is `TestCase` or `TestSupport` is pinned wherever it lives, not only when its file is a detected test file; this covers Rust `#[cfg(test)]` functions inside production files. With the key set to `false`, such symbols move like any other symbol. Production symbols inside detected test files are still pinned by the file rule.

Source-to-test relationships are derived from immutable analysis-start paths using exact templates. Templates admit only the `{dir}` and `{stem}` placeholders. Custom rules supplement optional language built-ins. TypeScript and JavaScript conventions cover `src` and `source` production roots, `spec`, `test`, and `tests` test roots, colocated `.spec` and `.test` files, and supported JS/TS extensions. JavaScript rules remain dormant unless the selected adapter discovers those files. Python conventions cover `test_{stem}.py` and `{stem}_test.py` under `test`, `tests`, and colocated source roots. Rust has no built-in file mirror because its integration and inline test conventions do not provide a reliable one-to-one mapping.

An accepted source-file move attempts every exact test mirror as a best-effort follower. A successful follower applies the same relative folder change under its test root and participates in final capacity, path, anchor, naming, candidate ordering, and score totals exactly once. It is attached to the primary recommendation rather than narrated independently.

A rejected follower does not veto the source move. It remains in its original folder and is attached as a blocked mirror with the first deterministic reason: ambiguous mapping, namespace boundary, capacity, or path collision. [ADR-17](adr-17-relocations-stay-inside-their-package.md) adds the `packageBoundary` reason, checked before `namespaceBoundary`. Earlier relocations cannot create new source/test relationships.

Each primary `Move` in analysis results carries `mirrors` and `blockedMirrors`, introduced in result schema version 7. A successful mirror records `sourcePath`, `path`, `from`, and `to`; a blocked mirror records `sourcePath`, `path`, `from`, `intendedTo`, and `reason`. Human output renders source files, mirrored tests, and blocked mirrors as one linked recommendation. File counts include successful followers, while recommendation counts count the linked group once.

## 🔀 Alternatives considered

**Pin test symbols only inside detected test files.** Rejected because test-polarity symbols inside production files, such as Rust `#[cfg(test)]` functions, could then be proposed as independent symbol moves.

**Remove pinned tests from analysis.** Rejected because their dependencies and structural findings remain valid evidence even when their placement is not independently actionable.

**Match tests by basename.** Rejected because common filenames create ambiguous or incorrect source/test pairings across modules.

**Veto a source move when any mirror is blocked.** Rejected because a valid source improvement should not depend on every test follower satisfying unrelated hard constraints.

**Infer a Rust default mirror.** Rejected because Rust commonly combines inline tests and integration tests without a dependable path-level one-to-one convention.

## ⚖️ Consequences

Profiles can independently freeze generated, vendored, test, or other policy-owned files and declarations without hiding their graph evidence. Test-polarity symbols stay with their code wherever they live unless a profile sets `pin-detected-test-symbols = false`. Exact nested paths disambiguate repeated basenames, and one source can carry several linked tests in a single recommendation.

Best-effort following can leave a source and test temporarily unmirrored when a hard constraint blocks the test; schema and human output make that consequence explicit. Custom template mistakes fail configuration validation instead of silently degrading to basename inference. The mirror fields were introduced in result schema version 7; current readers accept only the current schema version, as [ADR-17](adr-17-relocations-stay-inside-their-package.md) records.
