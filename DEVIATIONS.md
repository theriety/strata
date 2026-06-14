# Deviations

### D-1: Full RST-CORE-04 lint table replaces the spec's minimal one
- **When**: Commit 1 (feat(ir): scaffold workspace and snapshot contract)
- **Draft said**: workspace `[workspace.lints.clippy]` denies only `clippy::all`
- **What I did instead**: denied `clippy::all` + `clippy::pedantic` with the curated allow-list and deny-without-exception list mandated by RST-CORE-04 / RST-TOOL-04
- **Reason**: standard-violation
- **Impact on spec**: surface-change
- **Severity**: minor

### D-2: `reason` lint-table key expressed as trailing comment
- **When**: Commit 1
- **Draft said**: n/a (spec did not include curated allows)
- **What I did instead**: cargo 1.96 rejects a `reason = "..."` key in `[lints]` entries with an "unused manifest key" warning, so each curated allow carries its RST-CORE-03 justification as a trailing `# reason:` comment instead
- **Reason**: wrong-integration
- **Impact on spec**: none
- **Severity**: minor

### D-3: Added rust-toolchain.toml and bacon.toml
- **When**: Commit 1
- **Draft said**: spec file tree lists only Cargo.toml and crate sources
- **What I did instead**: added `rust-toolchain.toml` (RST-TOOL-01) pinning channel + rustfmt/clippy and `bacon.toml` (RST-TOOL-03) as required toolchain pins
- **Reason**: standard-violation
- **Impact on spec**: surface-change
- **Severity**: minor

### D-4: Dropped the `TreeError::Cycle` variant as provably redundant
- **When**: Commit 1
- **Draft said**: `ContainerTree::validate` checks both strict parent-level ascent and that "parent links form a forest (no cycles)" via BFS
- **What I did instead**: kept strict per-edge level ascent, which makes the parent links a forest by construction (a finite strictly-increasing level chain cannot revisit a node), so a separate cycle pass is unreachable dead code and was removed
- **Reason**: standard-violation
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-5: Used `cargo test` because `cargo nextest` is not installed
- **When**: Commit 1
- **Draft said**: n/a (RST-TOOL-02 mandates `cargo nextest run`)
- **What I did instead**: `cargo-nextest` is not on PATH in this environment, so tests were run with `cargo test --workspace`; `bacon.toml` still wires the canonical `cargo nextest run` job
- **Reason**: missing-dep
- **Impact on spec**: none
- **Severity**: minor

### D-6: Defined adapter contract auxiliary types
- **When**: Commit 1
- **Draft said**: `adapter.rs` references `SourceFile`, `ParseTree`, and `AdapterError` without defining them
- **What I did instead**: defined minimal `SourceFile`, `ParseTree`, and a typed `AdapterError` (thiserror) so the `Adapter` trait compiles as the contract every later adapter crate depends on
- **Reason**: stale-symbol
- **Impact on spec**: surface-change
- **Severity**: minor
