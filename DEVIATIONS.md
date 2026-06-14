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

### D-7: `Interner::resolve` returns `Option<&SmolStr>` instead of `&SmolStr`
- **When**: Commit 2 (feat(core): CSR graph, Tarjan condensation, longest-path layering)
- **Draft said**: `pub fn resolve(&self, id: u32) -> &SmolStr` (infallible, would panic on unknown id)
- **What I did instead**: `pub fn resolve(&self, id: u32) -> Option<&SmolStr>` returning `None` for unknown ids
- **Reason**: standard-violation
- **Impact on spec**: surface-change
- **Severity**: minor

### D-8: CSR edge weights use baked-in default kind weights
- **When**: Commit 2 (feat(core): CSR graph, Tarjan condensation, longest-path layering)
- **Draft said**: per-edge weight = kind weight × binder confidence, with `build_csr(snapshot, filter)` taking no config
- **What I did instead**: kind weights come from the documented `[weights]` defaults (value-import/call 1.0, inheritance 1.5, type-reference 0.3, re-export 0.0); config plumbing arrives in a later slice
- **Reason**: wrong-integration
- **Impact on spec**: none
- **Severity**: minor

### D-9: Added `proptest` and `criterion` dev-dependencies
- **When**: Commit 2 (feat(core): CSR graph, Tarjan condensation, longest-path layering)
- **Draft said**: scope includes "property tests and a 100k-node release benchmark" without naming crates
- **What I did instead**: added `proptest` (property tests) and `criterion` (benchmark) to `[workspace.dependencies]` and the core crate's `[dev-dependencies]`
- **Reason**: missing-dep
- **Impact on spec**: none
- **Severity**: minor

### D-10: Defined the cluster phase's shared support types
- **When**: Commit 4 (feat(core): multilevel acyclic clustering with capacity vetoes)
- **Draft said**: `coarsen.rs` / `seed.rs` / `refine.rs` reference `LevelCaps`, `Partition`, and `GainFn` without defining them
- **What I did instead**: defined `ClusterId`, `LevelCaps` (defaults 15/12/10/12 from FR-1), `Partition` (assignment + per-cluster sizes), and `GainFn` (per-node token sets + α/β) in `cluster.rs`, plus a `SeedLevel` selector so each pass picks the right cap
- **Reason**: stale-symbol
- **Impact on spec**: surface-change
- **Severity**: minor

### D-11: Matching/seeding adapted to the repo's dependent→dependency edge orientation
- **When**: Commit 4
- **Draft said**: the coarsen matching rule is "safe when layer(v) - layer(u) <= 1" for an edge u ~> v, implying edges climb layers (target higher)
- **What I did instead**: this crate's CSR edges point from dependent to dependency with `layer(source) > layer(target)` (per commit 2's layering), so the screen is `layer(u) - layer(v) <= 1` with an explicit two-hop probe; seeding walks descending-layer (topological) order so contiguous clusters stay acyclic
- **Reason**: wrong-integration
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-12: FM gain accounts for incoming edges via a reverse adjacency
- **When**: Commit 4
- **Draft said**: the incremental acyclicity veto maintains a quotient order with "kahn-style local repair" without specifying edge direction handling
- **What I did instead**: the CoarseGraph carries only a forward CSR, so `QuotientEdges` builds a predecessor list once and a move's deltas adjust both the moved node's outgoing and incoming quotient edges; acyclicity is checked by Kahn over the small prospective quotient edge set
- **Reason**: wrong-integration
- **Impact on spec**: none
- **Severity**: minor

### D-13: Followed the real `Adapter` trait contract rather than the draft's free-function API
- **When**: Commit 8 (feat(adapter-typescript): swc parse and bind to IR fragments)
- **Draft said**: a free `parse(files) -> Vec<ParsedModule>` / `bind(modules) -> ...` API returning `ParsedModule` directly, with an `AdapterParseFailure` error type
- **What I did instead**: implemented the actual `strata_ir::Adapter` trait — `parse(&self, &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError>` and `bind(&self, Vec<ParseTree>) -> Result<IrFragment, AdapterError>` — threading the `ParsedModule` summary through the opaque `ParseTree.payload` as canonical JSON; errors use the real `AdapterError::{Parse, Bind}` variants
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-14: `bind` takes a `tsconfig` alias table as an explicit parameter
- **When**: Commit 8
- **Draft said**: binding resolves `tsconfig` `paths` aliases without specifying where the alias table comes from
- **What I did instead**: the `TypeScriptAdapter` reads `root/tsconfig.json` once at construction (`tsconfig::load_aliases`) and passes the resulting `BTreeMap<SmolStr, SmolStr>` into `bind::bind(&modules, root, aliases)`, keeping the binder a pure function of its inputs (no filesystem reads at bind time)
- **Reason**: missing-symbol
- **Impact on spec**: surface-change
- **Severity**: minor

### D-15: Pure barrel re-exports produce no edges under the per-symbol node model
- **When**: Commit 8
- **Draft said**: re-exports (`export ... from '...'`) are emitted as-is as re-export edges
- **What I did instead**: a re-export edge requires both a resolved target export and a *local* node to anchor the source end; a pure barrel file (e.g. `index.ts` with only re-exports and no local declarations) has no local node, so it emits no edge — matching the main spec's intent that barrels become invisible to the solver. Re-export edge emission is otherwise implemented for files that both re-export and declare locally
- **Reason**: behavior-change
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-16: Spike loads the rust-analyzer sysroot to measure full resolution
- **When**: Commit 9 (spike(adapter-rust): ra_ap binder feasibility (AD-4))
- **Draft said**: "load the fixture workspace via ra_ap_load_cargo" and walk references, with no mention of sysroot configuration
- **What I did instead**: set `CargoConfig.sysroot = Some(RustLibSource::Discover)` so std-library references (e.g. the `ToString::to_string` blanket-trait method call) resolve; with the default no-sysroot config the binder reached only 94.1% (the single std method-call missed), masking that the cross-crate (AD-4) edges themselves were already 100%
- **Reason**: wrong-integration
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-17: Outer workspace gains the crate as a member and excludes the fixture workspace
- **When**: Commit 9
- **Draft said**: showed only `crates/adapter-rust/{Cargo.toml, src/bin/spike.rs, tests/fixtures/workspace/}` without addressing the root `Cargo.toml`
- **What I did instead**: added `crates/adapter-rust` to `[workspace].members` and added `exclude = ["crates/adapter-rust/tests/fixtures/workspace"]` so the self-contained 3-crate fixture (loaded at runtime by the spike) is not treated as a member of the outer workspace
- **Reason**: arch-conflict
- **Impact on spec**: surface-change
- **Severity**: minor

### D-18: Peak RSS measured in-process via getrusage instead of an external launcher
- **When**: Commit 9
- **Draft said**: print "peak rss" and list "memory bounded" as an exit criterion; an earlier source comment offloaded RSS to a launcher's `/usr/bin/time -l`
- **What I did instead**: added `libc` and measure `ru_maxrss` from `getrusage(RUSAGE_SELF)` in-process (bytes on macOS, KiB on Linux), print `peak_rss_mib` and enforce it against a 4096 MiB budget in the go/no-go gate — no external launcher is committed or required; an unknown reading fails the gate (validated against `/usr/bin/time -l`: 520 MiB matched exactly)
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-19: Visibility and projection entry points take the data the spec signatures omit
- **When**: Commit 6 (feat(core): LCA visibility derivation and test projection)
- **Draft said**: `derive_visibility(tree: &ContainerTree, edges: &[Edge])`, `project(candidate: &mut Candidate, polarity: &[Polarity])`, and `detect_convention(package: &PackageView)` over undefined `Candidate`/`PackageView` types
- **What I did instead**: added a `nodes: &[Node]` parameter to `derive_visibility` (an `Edge` carries only `NodeId`s, so the node→container map and declared visibility must come from the node list) and a `convention: SpecConvention` parameter to `project`; defined the local view/result DTOs (`VisibilityResult`, `DerivedScope`, `Finding`, `Candidate`, `PackageView`, `Violation`, ...) the spec referenced but never declared, mirroring how `pack.rs` defines its `FolderView`
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-20: Scoring consumes a pre-extracted candidate view rather than a whole tree
- **When**: Commit 7 (feat(core): objective scoring and VI diversification)
- **Draft said**: `score(c: &Candidate, k: &Coefficients) -> ScoreBreakdown` over a `Candidate`/`Coefficients` the draft never defines (and a `Candidate` already exists in `project.rs` meaning something unrelated)
- **What I did instead**: defined `score::Candidate` as the pre-extracted projections the objective actually scores (`edges: Vec<ScoredEdge>` carrying each edge's kind/confidence/LCA level, `containers` child sizes for imbalance, `cohesion_groups` for naming, `path_cohesion`, `move_distance`) plus `Coefficients` with `anchored()`/`greenfield()` presets — mirroring how `pack.rs`/`visibility.rs` own only the view they consume (the established D-19 pattern); cohesion terms are pre-negated into the breakdown so it sums to `total`
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-21: Diversification takes a `Solver` abstraction instead of a `Snapshot`
- **When**: Commit 7
- **Draft said**: `diversify(snapshot: &Snapshot, cfg: &ModeConfig) -> ModeResult`, internally calling a `solve(seed)` that re-runs cluster + pack
- **What I did instead**: no single `solve` orchestrator exists yet in `core` (cluster/pack are separate phase functions, and wiring the full pipeline is out of this slice's two target files), so `diversify(solver: &impl Solver, cfg)` takes a `Solver` trait (`fn solve(&self, seed: u64) -> SolvedCandidate`) — keeping diversification a pure, testable function that owns oversampling/filtering/max-min-VI while the cluster+pack stack plugs in later; `ModeConfig` also carries `base_seed`, `score_tolerance`, and `min_distance` the draft left implicit. `vi_distance` operates on `cluster::Partition`, the canonical induced-partition type
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-22: Rust binder takes (manifest, root) and the trait wraps it, like the TS adapter
- **When**: Commit 10 (feat(adapter-rust): syn parse and ra_ap bind to IR fragments)
- **Draft said**: `bind(files: Vec<ParsedFile>, manifest: &Path) -> Result<IrFragment, AdapterError>`
- **What I did instead**: free `bind(files: &[ParsedFile], manifest: &Path, root: &Path) -> Result<IrFragment, BindOutcome>` plus a `RustAdapter` that holds `(manifest, root)` and implements the IR `Adapter` trait (`parse(&[SourceFile])`/`bind(Vec<ParseTree>)`, which carries no manifest); the trait impl maps `BindOutcome` into `AdapterError::Bind` — mirroring the landed `TypeScriptAdapter` pattern. `root` is required to relate parsed repo-relative paths to vfs absolute paths and to name containers
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-23: Per-symbol SLOC is computed inline during parse; no SymbolId-keyed sloc() exists
- **When**: Commit 10
- **Draft said**: `sloc(file: &ParsedFile) -> Vec<(SymbolId, u32)>`
- **What I did instead**: the IR has no `SymbolId` (node ids are `NodeId`, assigned at bind, not parse), so SLOC is computed per declaration during parsing and stored on `Declaration.sloc`; the public surface is `production_sloc(source: &str) -> u32` over a source slice (mirroring the TS adapter's `sloc` module), and `bind` copies each declaration's SLOC onto its `Node.effective_size`. `cfg(test)` declarations carry zero production SLOC
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-24: Engine orchestration split into error.rs and snapshot.rs modules
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: lib.rs holds `StrataError`, `snapshot_from_root`, and `analyze` directly (3-file slice: config/result/lib)
- **What I did instead**: kept lib.rs to crate docs + `pub mod`/`pub use` only (RST-MODL-04) and placed logic in `error.rs`, `snapshot.rs`, and `analyze.rs` alongside `config.rs`/`result.rs`, matching the reference file tree which already lists a separate engine `snapshot.rs`
- **Reason**: standard-violation
- **Impact on spec**: surface-change
- **Severity**: minor

### D-25: AnalyzeResult uses a nested `modes { anchored, greenfield }` shape
- **When**: Commit 12
- **Draft said**: the result.rs toggle types `AnalyzeResult` with flat `anchored: ModeResult` and `greenfield: Option<ModeResult>` fields
- **What I did instead**: followed the main spec's authoritative usage example (`result.modes.anchored`) and the reference DTO interface, which nest both modes under a `modes` object with `summary` and `current` siblings; both modes are `Option<ModeResult>` so an unrequested mode is omitted
- **Reason**: wrong-integration
- **Impact on spec**: surface-change
- **Severity**: minor

### D-26: Config DTOs are engine-native serde structs, not core algorithm types
- **When**: Commit 12
- **Draft said**: `AnalyzeConfig` fields are typed as core's `LevelCaps`, `EdgeWeights`, `Coefficients`, `SolverLimits`, `DiversityConfig`
- **What I did instead**: defined engine-native `CapacityConfig`/`WeightsConfig`/`ObjectiveConfig`/`SolverConfig`/`DiversityConfig` serde structs mirroring `strata.toml` exactly (with kebab-case key renames and a `file` SLOC cap that `LevelCaps` lacks), converting into core types at use sites; core's tuned algorithm types do not derive serde and omit config-only fields
- **Reason**: arch-conflict
- **Impact on spec**: surface-change
- **Severity**: minor

### D-27: analyze returns the current tree as the sole candidate per mode
- **When**: Commit 12
- **Draft said**: `analyze` runs the full condense -> shatter -> layer -> cluster -> pack -> visibility -> project -> score -> diversify pipeline
- **What I did instead**: wired the deterministic structural phases that consume only a `Snapshot` (condensation for cycle findings, polarity matrix, LCA visibility findings, current-tree scoring, tree rendering, per-mode results); candidate-tree reconstruction (cluster/pack/project view-extraction glue) is not in this slice's file set, so each `ModeResult` returns the rendered current tree as its sole candidate, keeping `analyze` pure and deterministic. The full restructuring search is a follow-up
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: major

### D-28: analyze now runs the diversifying restructure search (supersedes D-27)
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: `analyze` runs the full condense -> shatter -> layer -> cluster -> pack -> visibility -> project -> score -> diversify pipeline returning up to k diverse candidates per mode
- **What I did instead**: wired the real search — a `PipelineSolver: Solver` runs `condense -> layer -> coarsen -> seed -> refine` per seed (seeded by a deterministic layer jitter so distinct seeds explore distinct local optima), `diversify` selects up to k max-min-VI candidates, and each surviving partition is reconstructed into a laminar candidate `ContainerTree` (one folder per cluster, files reparented by their symbols' majority cluster), scored under the mode coefficients, and narrated via `narrate_delta`; `pairwise_distance` is the VI matrix. The candidate reconstruction is a folder-level grouping rather than the full `pack`/`project` view-extraction (SLOC-cap file splitting and spec-follows-subject projection are not glued in this slice), so `conditional_splits` stays empty and tree depth is root->folder->file
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-29: narrate_delta groups by destination-container id, on changed parent path
- **When**: Commit 12
- **Draft said**: "group moves by destination container, attach the dominant reason per group"
- **What I did instead**: the prior attempt keyed groups by each moved container's OWN id (one entry per moved unit). Fixed to key by the candidate-tree PARENT (the destination container), detecting a move by a changed parent path and coalescing every unit relocated into one destination into a single `Move` whose `from`/`to` are the parent paths and `symbols` the moved leaf names; root moves key under a `u32::MAX` sentinel
- **Reason**: standard-violation
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-30: default `strata.toml` uses `[diversity]`, not the reference's `[diversify]`
- **When**: Commit 13 (feat(cli): commands, rendering, e2e fixtures, and parity test)
- **Draft said**: the reference default-config TOML names the diversification block `[diversify]`
- **What I did instead**: emitted the block as `[diversity]` in the shipped `strata.toml`, matching the `AnalyzeConfig.diversity` serde field (commit 12, D-26). Using `[diversify]` makes the engine reject its own default config with `CONFIG_INVALID: unknown field`, which broke the rust fixture's `analyze`/`violations` runs until corrected
- **Reason**: stale-symbol
- **Impact on spec**: surface-change
- **Severity**: minor
