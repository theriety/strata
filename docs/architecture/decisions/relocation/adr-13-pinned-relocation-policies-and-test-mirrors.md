# ADR-13: Pinned relocation policies and exact test mirrors

📌

Each profile's relocation policy can pin files and symbols in place without removing them from analysis, detected tests are pinned by default, and an accepted source move carries its exact test mirrors as best-effort followers.

- Status: `Accepted`
- Date: `2026-08-31`

## 🎯 Motivation

Optimizing test files and test-support declarations independently can produce recommendations that break the source/test layout readers expect. Matching a test to production code by basename is also ambiguous whenever separate modules use a common filename.

## 🧭 Context

Test files and test-support declarations participate in useful dependency, scoring, capacity, and cycle evidence.

A source relocation can have one or more test counterparts. Those tests should follow the source when the relationship is exact and the resulting placement is valid, without making the source recommendation depend on every follower succeeding.

## ✅ Decision

Each parameter profile owns a relocation policy. Repo-relative glob patterns can pin files against independent file moves and can prevent symbols from leaving or entering matching files. Detected test files and their symbols are pinned by default. Pinned participants remain in the graph and continue to contribute to dependencies, scoring, capacity, cycles, and findings. If one file in a strongly connected file component is pinned, the whole component is pinned.

Source-to-test relationships are derived from immutable analysis-start paths using exact templates. Templates admit only the `{dir}` and `{stem}` placeholders. Custom rules supplement optional language built-ins. TypeScript and JavaScript conventions cover `src` and `source` production roots, `spec`, `test`, and `tests` test roots, colocated `.spec` and `.test` files, and supported JS/TS extensions. JavaScript rules remain dormant unless the selected adapter discovers those files. Python conventions cover `test_{stem}.py` and `{stem}_test.py` under `test`, `tests`, and colocated source roots. Rust has no built-in file mirror because its integration and inline test conventions do not provide a reliable one-to-one mapping.

An accepted source-file move attempts every exact test mirror as a best-effort follower. A successful follower applies the same relative folder change under its test root and participates in final capacity, path, anchor, naming, candidate ordering, and score totals exactly once. It is attached to the primary recommendation rather than narrated independently.

A rejected follower does not veto the source move. It remains in its original folder and is attached as a blocked mirror with the first deterministic reason: ambiguous mapping, namespace boundary, capacity, or path collision. Earlier relocations cannot create new source/test relationships.

Analysis results use schema version 7. Each primary `Move` carries `mirrors` and `blockedMirrors`. A successful mirror records `sourcePath`, `path`, `from`, and `to`; a blocked mirror records `sourcePath`, `path`, `from`, `intendedTo`, and `reason`. Human output renders source files, mirrored tests, and blocked mirrors as one linked recommendation. File counts include successful followers, while recommendation counts count the linked group once.

## 🔀 Alternatives considered

**Remove pinned tests from analysis.** Rejected because their dependencies and structural findings remain valid evidence even when their placement is not independently actionable.

**Match tests by basename.** Rejected because common filenames create ambiguous or incorrect source/test pairings across modules.

**Veto a source move when any mirror is blocked.** Rejected because a valid source improvement should not depend on every test follower satisfying unrelated hard constraints.

**Infer a Rust default mirror.** Rejected because Rust commonly combines inline tests and integration tests without a dependable path-level one-to-one convention.

## ⚖️ Consequences

Profiles can independently freeze generated, vendored, test, or other policy-owned files and declarations without hiding their graph evidence. Exact nested paths disambiguate repeated basenames, and one source can carry several linked tests in a single recommendation.

Best-effort following can leave a source and test temporarily unmirrored when a hard constraint blocks the test; schema and human output make that consequence explicit. Custom template mistakes fail configuration validation instead of silently degrading to basename inference. Saved-result consumers must migrate to schema version 7.
