# Contributing to strata

This guide covers how to build the workspace, run the full test suite, regenerate the committed goldens, and pass the formatting and lint gates. It assumes you have read [README.md](README.md) for what `strata` does and [ARCHITECTURE.md](ARCHITECTURE.md) for how the crates fit together.

## Toolchain

The workspace pins its toolchain in [`rust-toolchain.toml`](rust-toolchain.toml): Rust `1.96.0` with the `rustfmt` and `clippy` components. `rustup` selects it automatically when you run any `cargo` command inside the repo, so no manual `rustup override` is needed.

Two optional tools make the inner loop faster:

- **[`cargo-nextest`](https://nexte.st)** — the canonical test runner (`cargo nextest run`). Install with `cargo install cargo-nextest`. Plain `cargo test` works as a fallback if it is not on `PATH`.
- **[`bacon`](https://dystroy.org/bacon)** — a background checker wired to this repo via [`bacon.toml`](bacon.toml). Install with `cargo install bacon`, then run `bacon` for a live `clippy` loop (`c` checks, `k` clippies, `t` tests).

## Workspace layout

`strata` is a seven-crate Cargo workspace ([`Cargo.toml`](Cargo.toml)). Source lives under `crates/<name>/src`, and tests live in three places per crate — inline `#[cfg(test)]` unit modules, crate-level `tests/` integration files, and fixture trees the integration tests drive.

| Crate | Role | Test surface |
|-------|------|--------------|
| `crates/ir` | The typed-graph IR contract every other crate depends on. | Inline unit tests in `src`. |
| `crates/core` | Solver core: Tarjan, layering, MFAS, clustering, packing, scoring. | Inline unit tests across `src` (the bulk of the algorithmic coverage). |
| `crates/engine` | Orchestration engine and public library surface (`analyze`, `snapshot_from_root`). | Inline unit tests in `src`. |
| `crates/cli` | The `strata` binary — a thin shell over `strata-engine`. | Inline unit tests plus the e2e suite under `tests/e2e`. |
| `crates/adapter-typescript` | TypeScript / TSX language adapter. | `tests/fixtures.rs` over `tests/fixtures`. |
| `crates/adapter-rust` | Rust language adapter. | `tests/fixtures.rs` over `tests/fixtures`. |
| `crates/adapter-python` | Python language adapter. | `tests/fixtures.rs` over `tests/fixtures`. |

The fixture cargo workspace under `crates/adapter-rust/tests/fixtures/workspace` is intentionally `exclude`d from the outer workspace — it is sample source the Rust adapter loads at runtime, not a member crate, so cargo must never build it.

## Running the test suite

Run everything across all seven crates:

```sh
cargo nextest run --workspace      # or: cargo test --workspace
```

This covers three tiers, from cheapest to most integrated:

1. **Unit tests** — inline `#[cfg(test)] mod tests` modules inside each crate's `src`. They pin individual algorithms and data structures (most live in `crates/core`).
2. **Adapter fixture tests** — `crates/adapter-{typescript,rust,python}/tests/fixtures.rs`. Each parses and binds a small fixture repo under its `tests/fixtures` directory, then asserts the emitted nodes, edges, and containers in a canonical, deterministic order — catching any drift in extraction, resolution, polarity, or SLOC.
3. **CLI end-to-end tests** — the two files under `crates/cli/tests/e2e`, described below. They drive the compiled `strata` binary the way CI would.

To run a single tier, scope by crate or by test target:

```sh
cargo test -p strata-core                         # one crate's unit tests
cargo test -p strata-adapter-rust                 # one adapter's fixtures
cargo test -p strata-cli --test parity            # the e2e parity + golden file
cargo test -p strata-cli --test cli_acceptance    # the e2e acceptance file
```

## The e2e suite

The CLI's end-to-end coverage lives in two files under `crates/cli/tests/e2e`, layered deliberately so each owns exactly one kind of oracle.

### `cli_acceptance.rs` — exit codes and structural invariants

The user-facing acceptance layer. It drives the *compiled* binary via `assert_cmd` across every command — `analyze`, `tree`, `report`, `diff`, and `violations` — and asserts behaviour through **exit codes and structural invariants**: the node/edge census, candidate counts, header presence, `move …` narration, the presence of a `[violation]` line. It deliberately never asserts the exact rendered bytes — those belong to the goldens.

Determinism is engineered, not hoped for: every analysis runs with an explicit non-existent `--config`, forcing the binary onto its built-in defaults regardless of the harness's working directory. An identical snapshot and config therefore yield byte-identical output, so the invariants never flake on ordering. Both μ-modes are exercised — anchored (μ > 0) and greenfield (μ = 0).

### `parity.rs` — library↔CLI parity and rendered-output goldens

Two responsibilities:

- **Parity (AD-5).** For each fixture it runs the real binary with `analyze --format json` and, in the same process, calls `snapshot_from_root` + `analyze` from `strata-engine`. The two must agree **byte-for-byte**: the CLI's JSON (minus its trailing newline) must equal `serde_json::to_vec` of the library result. Because `analyze` is pure and deterministic, the comparison is stable.
- **Goldens.** It then renders the downstream faces — `tree`, `diff`, `violations`, `report`, and `analyze --format summary` — against a saved result and asserts each is byte-identical to a committed golden file.

### Oracle layering

The three oracles never overlap, by design:

| Layer | Oracle | What it pins |
|-------|--------|--------------|
| **Goldens** (`parity.rs`) | The committed golden files | The single oracle for **rendered output** (`tree`, `diff`, `violations`, `report`, `analyze --format summary`). One bless path. |
| **Asserts** (`cli_acceptance.rs`) | Hand-written assertions | **Exit codes and structural invariants** only — never exact bytes. |
| **Parity** (`parity.rs`) | The library result | **Library↔CLI byte-equality** of the JSON face. |

## Fixtures and goldens

E2e fixtures live under `crates/cli/tests/e2e/fixtures`. Each top-level directory is a small, self-contained sample repo the suite analyzes:

```text
crates/cli/tests/e2e/fixtures
├── ts                 # single-package TypeScript
├── rust               # single-crate Rust
├── python             # single-package Python
├── workspace-rust     # multi-crate Rust workspace
├── nested-ts          # nested TypeScript layout
├── nested-python      # nested Python layout
├── cyclic             # carries a cycle hard violation
├── over-capacity      # carries an over-capacity finding
├── polarity-leak      # carries a production↛test polarity violation
└── goldens            # committed rendered-output goldens, one dir per fixture
```

The six structurally clean fixtures (`ts`, `rust`, `python`, `workspace-rust`, `nested-python`, `nested-ts`) are driven as a group; the three violation-focused fixtures (`cyclic`, `over-capacity`, `polarity-leak`) each gate one invariant and get their own dedicated parity and golden tests.

Goldens are kept in a parallel `goldens/<fixture>` tree, **outside** the fixture repos, so the analyzer never scans them as source. Each fixture directory holds the rendered faces it pins:

```text
crates/cli/tests/e2e/fixtures/goldens/ts
├── diff.txt
├── report.md
├── summary.txt
├── tree.txt
└── violations.txt
```

## The golden bless workflow

When a renderer change is intentional, the golden files must be regenerated. Set `STRATA_BLESS=1` and run the parity suite — each golden assertion then writes the current binary's output to disk instead of comparing against it:

```sh
STRATA_BLESS=1 cargo test -p strata-cli --test parity
```

A missing golden fails with `missing golden …; run with STRATA_BLESS=1 to generate it`, so a freshly added fixture is blessed the same way.

After blessing, **review the diff before committing**: `git diff crates/cli/tests/e2e/fixtures/goldens` should show only the changes your work was meant to produce. A golden change that you cannot explain is a regression, not a refresh. Never set `STRATA_BLESS=1` in CI — there the goldens are the oracle, not the output.

## Formatting and lint gates

Both gates must pass before a change lands. The repo ships no `rustfmt.toml`, so default `rustfmt` style applies.

```sh
cargo fmt --all -- --check     # formatting gate (CI form; drop --check to fix)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Clippy runs with `all` and `pedantic` denied workspace-wide (see the `[workspace.lints]` table in [`Cargo.toml`](Cargo.toml)). A small, individually-justified allow-list relaxes a handful of pedantic lints; everything else is `-D warnings`. On top of that, several lints are **denied without exception** because they catch real bugs — notably `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `indexing_slicing`, `float_cmp`, and `dbg_macro`. Surface failures through assertions and typed errors, never an `unwrap`/`expect` or a hard panic; the e2e tests follow this by returning sentinel values rather than panicking on spawn failure.

The `bacon` jobs mirror these gates exactly — `clippy` (the default job), `check` (`cargo check --workspace --all-targets`), and `test` (`cargo nextest run --workspace`) — so a green `bacon` session is a good proxy for a green CI run.

## Before you open a pull request

Run, in order:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace          # or cargo test --workspace
```

If you touched a renderer, re-bless the goldens and review the diff. If you changed the public library surface, confirm the parity test still passes — a library↔CLI mismatch means the JSON face drifted.
