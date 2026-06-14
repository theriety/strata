# strata

A read-only static-analysis tool that proposes a **hierarchical, acyclic module decomposition** for a codebase. `strata` parses a repository (TypeScript, Rust, or Python), builds an import/reference/call graph, breaks cycles, packs symbols into well-sized files and a clean container hierarchy, scores the result against a structural objective, and reports restructure candidates and structural violations.

It never writes to your source — every command only reads the repository and prints a report.

## Requirements

- **Rust 1.85+** (the workspace uses edition 2024).
- A **C/C++ toolchain and CMake**, required to build the bundled [HiGHS](https://highs.dev/) ILP solver used for exact cycle-breaking on small graphs.

## Install (local)

From a clone of this repository, install the `strata` binary onto your `PATH`:

```sh
cargo install --path crates/cli
```

Or build a release binary without installing — it lands at `target/release/strata`:

```sh
cargo build --release
```

## Usage — point at a repo, get a report

Run the full decomposition pipeline against any repository by pointing `--root` at it. On a terminal this prints a human-readable analysis report (violations + restructure candidates); piped or redirected, it emits JSON.

```sh
strata analyze --root path/to/repo
```

`--root` defaults to the current directory, so from inside a project you can simply run:

```sh
strata analyze
```

### Markdown report

`report` renders a saved analysis as Markdown. Produce the analysis JSON once, then render it:

```sh
strata analyze --root path/to/repo --format json --output analysis.json
strata report  --input analysis.json --output report.md
```

(omit `--output` on `report` to print the Markdown to stdout.)

### Recommended structures

Add `--show-suggestions` to print the best candidate's proposed structure for each mode directly in the summary face:

```sh
strata analyze --root path/to/repo --show-suggestions
```

The flag is a no-op for `--format json`, which already serializes every candidate tree.

### Useful options for `analyze`

| Flag | Meaning |
|------|---------|
| `--root <path>` | Repository root to analyze (default `.`). |
| `--config <path>` | Config file (default `./strata.toml`; built-in defaults apply when absent). |
| `--mode <anchored\|greenfield\|both>` | Stay close to the current layout, propose an unbiased ideal, or both. |
| `-k, --candidates <n>` | Number of candidate structures to produce per mode. |
| `--seed <n>` | Deterministic seed for reproducible candidates. |
| `--jobs <n>` | Parallelism for parsing and shattering (`0` = all cores; never affects results). |
| `--format <summary\|json>` | Force the output face (defaults to summary on a TTY, JSON when piped). |
| `--output <path>` | Write the result to a file instead of stdout. |
| `--show-suggestions` | Print the best candidate's recommended structure per mode (summary face only). |

## Commands

| Command | What it does |
|---------|--------------|
| `strata analyze --root <path>` | Run the full pipeline; emit violations plus restructure candidates. |
| `strata violations --root <path>` | Report structural violations only — the CI gate (see exit codes). |
| `strata report --input <analysis.json>` | Render a saved analysis as a Markdown report. |
| `strata tree --input <analysis.json>` | Draw the file/container tree of a candidate (or `--current` for the as-is structure). |
| `strata diff --input <analysis.json> <left> <right>` | Compare two structures (`current` or `mode/index`) as a narrated move list. |

Run `strata <command> --help` for the full flag list of any subcommand.

### Inspecting a saved analysis

`tree`, `diff`, and `report` all operate on the `AnalyzeResult` JSON written by `analyze --format json --output`, so a single analysis can be explored many ways without re-running the pipeline.

`tree` draws the file/container hierarchy of one candidate (or the current layout):

| Flag | Meaning |
|------|---------|
| `--input <path>` | `AnalyzeResult` JSON from a previous `analyze`. |
| `--mode <name>` | Mode to draw a candidate from (`anchored` or `greenfield`). |
| `--candidate <n>` | 1-based candidate index; omit to list the available candidates. |
| `--current` | Draw the current (as-is) structure instead of a candidate. |
| `--symbols` | List each file's symbols with their derived visibility. |
| `--depth <n>` | Truncate the tree at container depth `n`. |

`diff` narrates the moves between two structures, each named `current` or `mode/index`:

```sh
strata diff --input analysis.json current anchored/1
```

| Argument | Meaning |
|----------|---------|
| `--input <path>` | `AnalyzeResult` JSON from a previous `analyze`. |
| `<left>` | First structure reference (`current` or `mode/index`). |
| `<right>` | Second structure reference (`current` or `mode/index`). |

## CI gate

`violations` is the gating command. Use `--fail-on` with comma-separated `kind[:severity]` selectors to make `strata` exit non-zero when a matching structural violation is present:

```sh
strata violations --root path/to/repo --fail-on cycle,capacity:violation
```

Selector classes are `cycle`, `polarity`, `capacity`, and `visibility`, each optionally suffixed with a severity (`:violation` or `:borderline`).

| Flag | Meaning |
|------|---------|
| `--root <path>` | Repository root to analyze (default `.`). |
| `--config <path>` | Config file (default `./strata.toml`; built-in defaults apply when absent). |
| `--jobs <n>` | Parallelism for parsing (`0` = all cores). |
| `--fail-on <selectors>` | Comma-separated `kind[:severity]` selectors that trigger exit code `2`. |
| `--format <table\|json>` | Output face (default `table`). |

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success (or no gating violation matched). |
| `1` | Any error, including a usage/parse error. |
| `2` | Reserved exclusively for a `violations --fail-on` match. |

## Configuration

Every command reads `strata.toml` from the analysis root (override with `--config`). The file is optional: an empty file behaves identically to no file, and every key falls back to a built-in default. CLI flags override the config, which overrides the defaults. The sections are:

| Section | Controls |
|---------|----------|
| `[adapters]` | Which `languages` to run, plus `include`/`exclude` source globs. |
| `[analysis]` | Default `mode`, `candidates` (k), `seed`, and `jobs`. |
| `[capacity]` | Hard size caps: production SLOC per `file` and member counts per container level. |
| `[objective]` | Objective-term weights: `imbalance`, `naming`, `path`, and `anchor`. |
| `[weights]` | Per-edge-kind weights (`value-import`, `inheritance`, `call`, `type-reference`, `re-export`). |
| `[solver]` | `ilp-threshold` (max SCC size for the exact ILP) and `timeout-seconds`. |
| `[diversity]` | Candidate diversification: `seeds-per-candidate`, `score-tolerance`, `min-distance`. |
| `[tests]` | `helper-cap` for test-support files. |

The checked-in [`strata.toml`](strata.toml) documents every key alongside its default value.

## How it works

`strata` assembles a language-agnostic IR snapshot from per-language adapters, then runs a pure, deterministic decomposition pipeline (condense cycles, layer, cluster, score, diversify) over it — the engine never sees source code, only the snapshot. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate layout, the eleven-phase pipeline, the IR snapshot contract, and how the anchored and greenfield modes differ.
