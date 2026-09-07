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

### Comparing candidate layouts

Terminal summaries and Markdown reports show the same content in the same order: **Structural findings**, **Candidate layouts**, then **Advice**. Each candidate includes its baseline and resulting score, improvement, exact proposed moves, folder impacts, and before/after trees of affected branches. Scores compare only within the same parameter profile and project; lower is better.

Changed filenames in the trees end in `*`. Moved symbols appear beneath their source file in the before tree and their destination file in the after tree. Unchanged branches and symbols are explicitly omitted; this is a view of the affected analyzed files, not a complete listing of the repository. Candidate moves are proposals; the final Advice section separately explains their confidence.

Add `--verbose` for score-component deltas, numerical advice evidence and thresholds, and effective configuration:

```sh
strata analyze --root path/to/repo --format summary --verbose
strata report --input analysis.json --verbose --output report.md
```

Verbosity does not change JSON output or analysis results. Default reports retain all candidate moves, structural findings, profile agreement, review reasons, and conditional prerequisites.

### Reading relocation advice

`strata` separates relocation advice into two confidence groups:

- **Recommended** — the destination passes the selected profiles' evidence thresholds, and a strict majority of all executed profiles select that same destination. With the default two profiles, both must agree.
- **Review candidate** — the move is structurally safe but has weaker evidence, insufficient separation from another plausible destination, support from only some profiles, or conflicting destinations across profiles. Review candidates remain visible so uncertainty does not hide a potentially useful move.

Ordinary non-moves are omitted; there is no public `Rejected` group. Agreement between profiles supports a recommendation but is not independent proof, because the profiles evaluate the same underlying graph. A single-profile run can still recommend a move, but the move must pass that profile's evidence and ambiguity thresholds.

The evidence qualification is deterministic and language-neutral. It evaluates the immutable analysis-start graph before comparing profiles:

- **Unique owner** — the destination contains the sole explicit function or method that owns the symbol.
- **Role affinity** — the symbol's name agrees with the destination filename and existing symbols. Names are hints and can never authorize a move alone.
- **Source cohesion** — the move does not break a stronger conceptual cluster in the source file.
- **Destination cohesion** — multiple destination declarations support the symbol's role; one nearby declaration is deliberately limited evidence.
- **Producer evidence** — explicit owner affinities emitted by an adapter provide producer evidence that outweighs passive consumers.
- **Ambiguity margin** — the proposed destination must clearly beat the strongest conservative alternative supported by the analysis-start graph.
- **Architectural reach** — shared consumers favor their common ancestor rather than one consumer branch.

Hard structural guards remain profile-independent. Profiles may adjust only the soft evidence weights and thresholds used to qualify otherwise admissible moves.

The ambiguity comparison deliberately over-approximates alternatives from pass-start evidence: the current source, incident production-neighbor homes, explicit owner homes, and the common physical folder of consumers. It does not reconstruct every optimizer admission check. An implausible alternative may therefore demote a move to `Review candidate`, but ambiguity qualification cannot promote a move or remove it from the raw candidates.

### Useful options for `analyze`

| Flag | Meaning |
|------|---------|
| `--root <path>` | Repository root to analyze (default `.`). |
| `--config <path>` | Config file (default `./strata.toml`; built-in defaults apply when absent). |
| `--mode <anchored\|greenfield\|both>` | Select which parameter profile or profiles to execute. |
| `-k, --candidates <n>` | Override the candidate count for every selected parameter profile. |
| `--seed <n>` | Override the deterministic seed for every selected parameter profile. |
| `--jobs <n>` | Parallelism for parsing and shattering (`0` = all cores; never affects results). |
| `--format <summary\|json>` | Force the output face (defaults to summary on a TTY, JSON when piped). |
| `--output <path>` | Write the result to a file instead of stdout. |
| `--verbose` | Include numerical evidence, score-component deltas, and effective configuration in human-readable output. Also available on `report`; JSON is unchanged. |

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
| `--mode <name>` | Parameter profile to draw a candidate from (`anchored` or `greenfield`). |
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
| `[adapters]` | Which `languages` to run, `include`/`exclude` source globs, and physical source-root handling. |
| `[analysis]` | Selected parameter profiles and process-wide `jobs`. |
| `[profiles.anchored]` | Anchored candidate count, seed, capacities, objective, evidence qualification, dependency weights, solver, diversity, test policy, and relocation policy. |
| `[profiles.greenfield]` | The same complete parameter set for greenfield, independently configurable. |

The checked-in [`strata.toml`](strata.toml) documents every key alongside its default value.

Each profile owns the complete analysis policy. Both run against the same discovered snapshot; only adapter discovery and process-wide parallelism are global. The built-in defaults differ only in greenfield's `path = 0.0` and `anchor = 0.0`. Explicit nonzero greenfield values are honored.

```toml
[analysis]
profiles = ["anchored", "greenfield"]
jobs = 0

[profiles.anchored]
candidates = 3
seed = 42

[profiles.anchored.capacity]
file = 250
folder = 20
domain = 16
package = 15
package-group = 12

[profiles.anchored.objective]
imbalance = 0.1
naming = 0.3
path = 0.2
anchor = 1.0
dependency-only = 0.05
companion-separation = 0.05
capacity = 4.0

[profiles.anchored.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15

[profiles.anchored.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20

[profiles.anchored.weights]
value-import = 1.0
inheritance = 1.5
call = 1.0
type-reference = 0.3
re-export = 0.0
same-file-symbol = 1.0
same-file-type = 3.0

[profiles.anchored.solver]
ilp-threshold = 300
timeout-seconds = 60

[profiles.anchored.diversity]
seeds-per-candidate = 10
score-tolerance = 0.05
min-distance = 0.05

[profiles.anchored.tests]
helper-cap = 250
patterns = []
builtins = true

[profiles.anchored.relocation]
pin-detected-test-files = true
pin-detected-test-symbols = true
forbid-file-moves = []
forbid-symbol-moves = []

[profiles.anchored.relocation.test-mirroring]
enabled = true
builtins = true

[profiles.greenfield]
candidates = 3
seed = 42

[profiles.greenfield.capacity]
file = 250
folder = 20
domain = 16
package = 15
package-group = 12

[profiles.greenfield.objective]
imbalance = 0.1
naming = 0.3
path = 0.0
anchor = 0.0
dependency-only = 0.05
companion-separation = 0.05
capacity = 4.0

[profiles.greenfield.qualification]
minimum-evidence = 0.60
minimum-structural = 0.50
minimum-ambiguity-margin = 0.15

[profiles.greenfield.qualification.weights]
unique-owner = 0.10
role-affinity = 0.10
source-cohesion = 0.25
destination-cohesion = 0.25
producer-evidence = 0.10
architectural-reach = 0.20

[profiles.greenfield.weights]
value-import = 1.0
inheritance = 1.5
call = 1.0
type-reference = 0.3
re-export = 0.0
same-file-symbol = 1.0
same-file-type = 3.0

[profiles.greenfield.solver]
ilp-threshold = 300
timeout-seconds = 60

[profiles.greenfield.diversity]
seeds-per-candidate = 10
score-tolerance = 0.05
min-distance = 0.05

[profiles.greenfield.tests]
helper-cap = 250
patterns = []
builtins = true

[profiles.greenfield.relocation]
pin-detected-test-files = true
pin-detected-test-symbols = true
forbid-file-moves = []
forbid-symbol-moves = []

[profiles.greenfield.relocation.test-mirroring]
enabled = true
builtins = true
```

Relocation patterns are repo-relative globs. `forbid-file-moves` pins matching files against independent folder moves while leaving them in dependency, scoring, capacity, cycle, and finding calculations. `forbid-symbol-moves` prevents a symbol move when either its analysis-start source file or proposed destination file matches. Detected tests and their symbols are pinned independently by default, but a test can still follow an accepted source move as a mirror.

Test mirroring uses exact analysis-start paths rather than basenames. Add custom rules with `{dir}` and `{stem}` captures:

```toml
[[profiles.anchored.relocation.test-mirroring.rules]]
source = "src/{dir}/{stem}.ts"
tests = [
  "spec/{dir}/{stem}.spec.ts",
  "test/{dir}/{stem}.test.ts",
  "src/{dir}/{stem}.spec.ts",
]
```

Templates must be non-empty repo-relative paths and may use only `{dir}` and `{stem}`. Empty captured directories are normalized without duplicate separators. Invalid globs, unknown or unmatched placeholders, paths containing `..`, absolute paths, and duplicate source templates fail configuration validation at the precise profile and rule key.

With `builtins = true`, TypeScript conventions match `src` and `source` files to `.spec` and `.test` files under `spec`, `test`, `tests`, or the same source root. The built-in table also defines the equivalent `js`, `jsx`, and `tsx` extensions; JavaScript rules remain dormant unless the selected adapter discovers those files. Python conventions match `test_{stem}.py` and `{stem}_test.py` under `test`, `tests`, and colocated source roots. Rust has no built-in mirror; add a custom rule only when a repository has a reliable one-to-one convention.

When a source moves, every exact mirror is attempted as a best-effort follower. Successful mirrors affect final scoring and capacity once and appear under the source recommendation instead of as standalone moves. A mirror blocked by ambiguous mapping, namespace boundary, capacity, or path collision remains in place and is reported without vetoing the source move. Set `enabled = false` to disable following, or `builtins = false` to use only custom rules. Define relocation settings separately under each profile when their policies should differ.

The former `analysis.mode`, `analysis.candidates`, `analysis.seed`, and top-level analysis-policy sections are invalid. `--mode` remains a CLI compatibility selector: it chooses which parameter profiles execute but does not define their parameters.

Saved analysis JSON uses result schema version 8. The top-level `advice` object contains `recommended` and `reviewCandidates`; raw per-profile candidates remain available for inspection. Each advice item records `destination`, `supportingProfiles`, `qualifiedProfiles`, `absentProfiles`, `conflictingDestinations`, `assessments`, and `reviewReasons`. An assessment includes all six raw evidence signals, the weighted and structural scores, the ambiguity margin and best alternative, the effective thresholds, and the qualification result. There is no serialized rejected group.

Each score breakdown includes `dependencyOnly` and `companionSeparation`. The latter is the directional charge for keeping a conservatively matched signature type away from its immutable owner file. Each primary file `Move` also has `mirrors` and `blockedMirrors`: successful mirrors contain `sourcePath`, `path`, `from`, and `to`, while blocked mirrors contain `sourcePath`, `path`, `from`, `intendedTo`, and `reason`. Earlier saved result versions are not accepted by `report`, `tree`, or `diff`.

## How it works

`strata` assembles a language-agnostic IR snapshot from per-language adapters, then runs a pure, deterministic decomposition pipeline (condense cycles, layer, cluster, score, diversify) over it — the engine never sees source code, only the snapshot. See [docs/architecture/overview.md](docs/architecture/overview.md) for the crate layout, the eleven-phase pipeline, the IR snapshot contract, and how the anchored and greenfield parameter profiles differ.
