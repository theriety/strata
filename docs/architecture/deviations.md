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
- **What I did instead**: kind weights come from the documented `[weights]` defaults (value-import/call 1.0, inheritance 1.5, type-reference 0.3, re-export 0.0); the `[weights]` table remains unplumbed as of commit 13 — see D-33 for the full inert-key inventory
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
- **What I did instead**: followed the main spec's authoritative usage example (`result.modes.anchored`) and the reference DTO interface, which nest both modes under a `modes` object alongside `current`; the reference names the census sibling `stats { …, filesByLanguage }`, but the implementation deliberately calls it `summary` — a rename kept for API stability rather than corrected to match the reference. Both modes are `Option<ModeResult>` so an unrequested mode is omitted
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
- **What I did instead**: wired the real search — a `PipelineSolver: Solver` runs `condense -> layer -> coarsen -> seed -> refine` per seed (seeded by a deterministic layer jitter so distinct seeds explore distinct local optima), `diversify` selects up to k max-min-VI candidates, and each surviving partition is reconstructed into a laminar candidate `ContainerTree` (one folder per cluster, files reparented by their symbols' majority cluster), scored under the mode coefficients, and narrated via `narrate_delta`; `pairwise_distance` is the VI matrix. The candidate reconstruction is a folder-level grouping rather than the full `pack`/`project` view-extraction (SLOC-cap file splitting and spec-follows-subject projection are not glued in this slice), so `conditional_splits` stays empty. Candidate trees span the full five-level chain (packageGroup -> package -> domain -> folder -> file): each cluster's chain names are the plurality parent-directory prefix of its member files taken at one, two, and three segments, so a path shallower than three directories repeats its deepest directory name at the remaining levels (e.g. `pkg/pkg/pkg`) and a root-level file hangs under a synthetic `workspace` chain — the same convention the adapters use for the current tree, so an identical layout narrates as no moves
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor
- **Update (2026-07-06, REVISED)**: the search now runs the full multilevel path — weighted SCC-condensation quotient (symbol SCCs, not the spec's file graph; files are reparented afterwards by their symbols' cluster) → heavy-edge coarsening under the folder capacity cap → topological seeding → FM refinement with uncoarsening projection at every chain level → per-level hierarchy (folders → domains → packages → package groups via weighted quotient graphs) → J(T) polish sweep under the full objective. `conditional_splits` is now populated: any SCC whose production SLOC exceeds the file cap carries a split estimated by ceil-packing (`production_sloc.div_ceil(cap)` resulting files), shared verbatim across every candidate in both modes. `pack` itself (real SLOC-cap file splitting) remains the one unglued phase. The `pkg/pkg/pkg` duplicated-segment chains are gone: elected container names are canonicalized (`extend_under` — a child key that fails to path-extend its parent contributes only its final segment) and the tree face renders incremental names
- **Correction (2026-08-22)**: as of `d1916fc` the search quotient condenses the FILE graph (`build_file_graph` in `crates/engine/src/analyze/relocation/solver.rs`, invoked from the pipeline solver's constructor; partition coverage asserts "every file scc"); the symbol-graph Tarjan survives for MFAS findings only. The 2026-07-06 wording above predates that switch.

### D-29: narrate_delta groups by destination-container id, on changed parent path
- **When**: Commit 12
- **Draft said**: "group moves by destination container, attach the dominant reason per group"
- **What I did instead**: the prior attempt keyed groups by each moved container's OWN id (one entry per moved unit). Fixed to key by the candidate-tree PARENT (the destination container), detecting a move by a changed parent path and coalescing every unit relocated into one destination into a single `Move` whose `from`/`to` are the parent paths and `symbols` the moved leaf names; root moves key under a `u32::MAX` sentinel
- **Reason**: standard-violation
- **Impact on spec**: behavior-change
- **Severity**: minor
- **Update (2026-07-06, REVISED)**: `narrate_delta` is deleted from the library surface — it matched container ids across two unrelated interning spaces, which produced the garbage golden moves. Narration is now file-identity based (`engine/narrate.rs`): both trees are indexed at file level by full path (the stable identity), a file moves iff its folded parent path differs, and groups key on (destination folder, followsSubject) with fully deterministic ordering

### D-30: default `strata.toml` uses `[diversity]`, not the reference's `[diversify]`
- **When**: Commit 13 (feat(cli): commands, rendering, e2e fixtures, and parity test)
- **Draft said**: the reference default-config TOML names the diversification block `[diversify]`
- **What I did instead**: emitted the block as `[diversity]` in the shipped `strata.toml`, matching the `AnalyzeConfig.diversity` serde field (commit 12, D-26). Using `[diversify]` makes the engine reject its own default config with `CONFIG_INVALID: unknown field`, which broke the rust fixture's `analyze`/`violations` runs until corrected
- **Reason**: stale-symbol
- **Impact on spec**: surface-change
- **Severity**: minor

### D-31: `violations` runs the full analyze pipeline, not a structural-only pass
- **When**: Commit 13 (feat(cli): commands, rendering, e2e fixtures, and parity test)
- **Draft said**: `violations` derives findings from the structural phases only (condensation, polarity, visibility, capacity) with no clustering and no seed involvement
- **What I did instead**: the command calls the same engine `analyze` entry point as the `analyze` command, so the seeded cluster/diversify search runs on every invocation; the table face and the exit gate consume only `result.current.violations` (which the structural phases fully determine), but the json face emits the entire `AnalyzeResult` — candidates included — byte-identical to `analyze --format json`
- **Reason**: arch-conflict
- **Impact on spec**: surface-change
- **Severity**: minor

### D-32: Cycle break suggestions are never populated; `[solver]` is inert
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: cycle violations carry MFAS-derived `breakSuggestions` — ILP-exact (`exact: true`) under the `ilp-threshold`, ELS heuristic above it — honoring the `[solver]` limits (FR-3, NFR-2)
- **What I did instead**: the engine emits every cycle violation with `breakSuggestions: []`; core's MFAS solver stack is never invoked from `analyze`, so no suggestion, exact or heuristic, is ever produced and the `[solver]` table drives nothing. Pinned by the guard test `should_keep_break_suggestions_and_conditional_splits_empty_per_d28`, which fails loudly the day the solver lands
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: major
- **Update (2026-07-06, RESOLVED)**: MFAS is wired. `solve_cycles` runs core's `shatter` once per multi-member SCC (sequentially, in input order — `shatter_all`'s single weight table collides on per-SCC local edge numbering) under the configured `[solver]` limits; cycle violations carry real `breakSuggestions` (symbol names via SCC members, summed pair weights, `exact` fanned from the per-set ILP flag) and the detail reads `{n}-symbol cycle; break {src} -> {tgt} (w=…, exact|heuristic)`. The ILP model pins `threads = 1` for determinism. Caveat: an ILP timeout degrades that SCC to the heuristic break set (accepted — real SCCs solve in milliseconds against the 60s config default). The `should_keep_break_suggestions_and_conditional_splits_empty_per_d28` guard test is retired

### D-33: Several documented config keys parse and validate but drive nothing
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: `strata.toml` keys configure the run — `[objective]` λ/α/β/μ shape the score, `[weights]` shape edge weights, `[solver]` bounds the suggestion search, `[tests].helper-cap` caps helper fan-in, `[diversity].seeds-per-candidate` widens oversampling, `[analysis].jobs` sets parallelism
- **What I did instead**: all of these parse, type-check, and reject unknown fields (`deny_unknown_fields`), but none is read downstream: scoring uses the hardcoded `Coefficients::anchored()`/`greenfield()` presets, edge weights are the baked-in defaults (D-8), the solver never runs (D-32), `helper-cap` and `seeds-per-candidate` have no consumer, and analysis is single-threaded regardless of `jobs` (which is why `--jobs 1` and `--jobs 8` are byte-identical). Validation without effect makes dead keys look honored; wiring `[objective]` is deferred to a dedicated slice by decision
- **Reason**: wrong-integration
- **Impact on spec**: behavior-change
- **Severity**: major
- **Update (2026-07-06, NARROWED)**: `[objective]`, `[weights]`, `[solver]`, `[diversity].seeds-per-candidate`, and `[analysis].jobs` are all live — score coefficients feed every `score()` call plus the polish pass and GainFn α/β; `[weights]` builds a `KindWeights` consumed by build_csr, the cut term, MFAS edge weights, and narration pull weights; `[solver]` bounds shatter; seeds-per-candidate replaces the hardcoded k×10 oversample; jobs installs a scoped rayon pool. The only remaining inert key is `[tests].helper-cap`: test-polarity nodes carry zero production SLOC, so the helper fan-in cap has nothing to measure until per-test SLOC attribution exists

### D-34: Delta narration's reason and followsSubject are constants (extends D-29)
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: each move group carries the dominant reason for the group, and spec moves link to their subject via `followsSubject` (FR-10)
- **What I did instead**: `narrate_delta` stamps every group with the literal reason `cohesion gain` and `followsSubject: null` — reason attribution needs per-move gain provenance and the subject linkage needs the projection glue, neither of which is wired (the same reduction as D-28)
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor
- **Update (2026-07-06, RESOLVED)**: kinds, reasons, and `followsSubject` are computed in the new narration. MoveKind is folder-granular (a destination receiving from 2+ source folders → Merge; one source scattering to 2+ destinations → Split; else Move); reasons attribute in precedence order (`follows {subject}`, over-cap folder relief, `pulled by {partner} (w …)` from summed both-direction edge weight to destination residents, naming cohesion ≥ 0.5 mean pairwise Jaccard, `regrouped by clustering` fallback); a moved all-test file whose dominant production subject lands in the same candidate folder gets `followsSubject` and the diff face prints the indented `follows` line. Granularity stays folder-level until `pack` lands

### D-35: Candidate-vs-candidate diff renders stored narration, not a computed diff
- **When**: Commit 13 (feat(cli): commands, rendering, e2e fixtures, and parity test)
- **Draft said**: `diff <left> <right>` narrates the delta between the two referenced structures (FR-10)
- **What I did instead**: the candidate-vs-candidate arm prints the right candidate's stored `deltaNarration` (its delta versus the *current* tree) plus the stored intra-mode VI distance; no two-candidate structural diff is computed, and a cross-mode pair simply omits the VI line. Both refs are validated — an out-of-range left ref exits 1 with `CANDIDATE_NOT_FOUND`
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-36: Candidate trees copy the current tree's visibility findings
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: visibility is re-derived per candidate so each proposed structure reports its own over-exports (FR-8)
- **What I did instead**: candidate construction copies the current tree's derived visibility instead of re-running the LCA derivation against the candidate's container assignment — a candidate that would fix (or introduce) an over-export still reports the current tree's findings
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor
- **Update (2026-07-06, RE-AFFIRMED)**: still holds after the method redesign — candidates continue to copy the current tree's visibility findings; per-candidate re-derivation remains future work

### D-37: Naming and path cohesion terms are inert in scoring
- **When**: Commit 12 (feat(engine): orchestration, config, and public library surface)
- **Draft said**: the objective J(T) includes naming-cohesion and path-cohesion terms alongside cut, imbalance, and anchor
- **What I did instead**: the score-view extraction passes `cohesion_groups: []` and `path_cohesion: 0.0` for every tree, so the naming and path terms render `-0.0000` everywhere and never influence ranking; cut, imbalance, and anchor are the live terms
- **Reason**: arch-conflict
- **Impact on spec**: behavior-change
- **Severity**: minor
- **Update (2026-07-06, RESOLVED)**: both terms are live. Naming cohesion draws per-folder token sets from file basenames (case/separator split — the tokenizer shared with the narration naming-cohesion reason); path cohesion is the SLOC-weighted fraction of files whose candidate folder key matches their current folder key. Greenfield forces β = μ = 0 per AD-2, so its path/anchor terms stay `-0.0000` by contract rather than by stubbing
- **Update (2026-08-22, RESCALED — WS-D FIX02)**: the scale defect itself is fixed. Cut was a raw weighted sum `Σ w(e)c(e)h(lca)` growing with repository size (measured 98.8–99.1% of |J| on large-app), so the bounded reality terms (imbalance, naming, path, anchor — each ≤ ~1) could never influence a ranking: every failing eval witness traced to cut drowning them. `cut_cost` now returns the edge-weight-share-weighted mean crossing height, `Σ w·c·h / (16·Σ w·c) ∈ [0,1]` — dimensionless and repository-size independent, with the divisor constant across a repository's candidates (the same edges only change where they cross), preserving intra-repo ranking by cut alone. Coefficient defaults are unchanged (λ 0.1, α 0.3, β 0.2, μ 1.0) and now act as relative weights between comparable 0..1 terms. Measured effects: anchored candidate 1 on the tearing and inversion fixtures is the identity layout (β/μ now outvote the churn's cut savings); every anchored face in the e2e suite narrates zero moves (uniformly conservative); constellation-ts's anchored face reports `optimal` where it previously reported `outscored` — the misplacement repairs cost more in path cohesion than their normalized cut saves, and greenfield still outscores and names them (its acceptance test now asserts the mode split deliberately). Goldens re-blessed for rescaled figures and the conservative anchored faces

### D-38: No oversized-symbol exemption in the capacity walk
- **When**: Commit 13 (feat(cli): commands, rendering, e2e fixtures, and parity test)
- **Draft said**: the Test Matrix's oversized-symbol row — a file dominated by a single symbol larger than the file SLOC cap is exempt from capacity findings, because the symbol cannot be split
- **What I did instead**: the capacity walk (engine `analyze`, relocated from the CLI) flags any file whose production SLOC exceeds the cap with no single-symbol exemption — a lone 400-SLOC function against the 250 cap is reported as a violation
- **Reason**: behavior-change
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-39: Discovery follows `.gitignore` and hard-skips `.git`/`node_modules`
- **When**: post-review hardening (user-directed, 2026-07-02)
- **Draft said**: file discovery honors the `[adapters]` include/exclude globs, with `.git`, `node_modules`, `target`, and `.venv` excluded by default config
- **What I did instead**: the walker additionally honors `.gitignore` files under the analyzed root (repo-local rules only — no parent, global, or `.git/info/exclude` sources, and no git checkout required), and skips `.git`/`node_modules` directories unconditionally so a config override can never re-include them; the default exclude globs remain as a redundant guard
- **Reason**: behavior-change (build output such as a compiled `lib/` tree polluted real-repo snapshots; hard-coding every build directory into config does not scale)
- **Impact on spec**: behavior-change — snapshots of repos whose `.gitignore` covers source-shaped build output gain fewer files (and a different hash) than the spec's glob-only discovery
- **Severity**: minor

### D-40: Result schema v2 extends the reference DTO with standing fields
- **When**: method redesign (2026-07-06)
- **Draft said**: the reference `AnalyzeResult` DTO (schemaVersion 1) carries per-mode candidates with score, breakdown, and moves only
- **What I did instead**: bumped `RESULT_SCHEMA_VERSION` to 2. `ModeResult` gains `currentScore`, `currentScoreBreakdown`, and `currentStanding` (`optimal | outscored | infeasible`, camelCase) computed per mode under that mode's coefficients, and every `Candidate` gains `improvement` (= currentScore − score; positive means the candidate beats the current layout). `read_result` rejects v1 files with the standard schema error. The spec's reference DTO has no equivalent fields
- **Reason**: behavior-change (reliability surface — the user must see whether applying a candidate actually helps)
- **Impact on spec**: surface-change
- **Severity**: minor

### D-41: Identity-candidate emission policy is a product decision the spec doesn't cover
- **When**: method redesign (2026-07-06)
- **Draft said**: nothing — the spec never states whether "change nothing" may be emitted as a candidate
- **What I did instead**: the identity assignment (each SCC mapped to the current folder of its dominant member file) always competes in the anchored pool, both raw and as a refine seed, which enforces the reliability invariant that anchored candidate 1 is never worse than current. It is emitted as a visible candidate — a verbatim clone of the current tree, zero moves, improvement `+0.0000` — only when the current layout is capacity-clean (no `Severity::Violation` capacity findings; borderline is fine), and the mode then reports standing `optimal`. A cap-violating current layout keeps identity as a scoring baseline only and reports standing `infeasible` with an engine-verified notice: the engine re-runs the capacity walk over the best candidate's tree and reports `best candidate resolves {resolved} of {M} capacity finding(s)` (serialized as the additive `bestCandidateCapacity` field, still schema v2), plus — when any remaining breach is file-level — a pointer that only conditional splits can fix breaches that exceed the file cap. Results lacking the field render the plain `current layout violates capacity caps` fallback. Cycles and polarity never screen emission (SCCs co-cluster in every layout; polarity is per-symbol). Greenfield never seeds from the current layout (rename invariance, NFR) but still reports currentScore/improvement/standing
- **Reason**: behavior-change (product decision)
- **Impact on spec**: behavior-change
- **Severity**: minor

### D-42: Violation ordering is engine-defined; the spec is silent
- **When**: probe retrospective (2026-07-06)
- **Draft said**: nothing — the spec never states an order for the violations list, and the engine previously emitted findings in internal collection order (kind-grouped, tree-walk order within a kind)
- **What I did instead**: `collect_violations` ends with a total-order sort applied once in the engine so every face (summary, violations table, JSON, report) inherits it: severity rank (Violation before Borderline), then kind rank (Cycle, Polarity, Capacity, Visibility — the previous emission order, minimizing churn), then location, then detail. `--fail-on` gating is order-independent, so behavior is presentation-only
- **Reason**: behavior-change (a real-repo probe surfaced ~40 findings in effectively arbitrary order; gating classes should surface first and reruns should be diffable)
- **Impact on spec**: surface-change — JSON and text faces list the same findings in a defined order
- **Severity**: minor

### D-43: Config discovery defaults to `<root>/strata.toml`, not the CWD
- **When**: probe retrospective (2026-07-06)
- **Draft said**: `--config` defaults to `strata.toml` (implicitly CWD-relative), and a missing config silently falls back to built-in defaults
- **What I did instead**: `--config` became optional; when absent it resolves to `<root>/strata.toml`, so `--root ~/other/repo` picks up that repo's own config. An explicitly passed path that does not exist warns on stderr (`warning: config {path} not found; using built-in defaults`) and still falls back to defaults — no hard error, preserving the documented fail-open contract
- **Reason**: behavior-change (analyzing a foreign repo from elsewhere silently ignored that repo's strata.toml — fail-open plus CWD-relative default made misconfiguration invisible)
- **Impact on spec**: behavior-change — runs invoked outside the analyzed root now discover the root-local config; explicit-missing configs gain a stderr warning
- **Severity**: minor

### D-44: Capacity binds in the objective and solver grain, reversing the "hard constraints never appear in J" doctrine
- **When**: WS-D defect burn-down FIX03 + QUAL-P3-2 (2026-08-23)
- **Draft said**: capacity stays a post-hoc findings label only (`walk_capacity`); the objective prices cut/imbalance/naming/path/anchor and hard vetoes cyclicity, and configured caps never feed scoring or search
- **What I did instead**: three coordinated changes. (1) Objective: `Coefficients.gamma = 4.0` in both mode presets multiplies a sixth term `capacity_pressure` = Σ over scoped containers of `max(0, transitive_files − budget)/budget`, where scoped containers are Folder|Domain only (matching eval's `is_scoped_container`), ancestors bind via upward accumulation over the flat candidate IR, and a zero budget disables the term. γ=4 makes one full breach unit (~1.8) dominate the max anchor cost so anchored relief wins J. (2) Config: `[capacity]` gains `folder`-scoped `ObjectiveConfig.capacity` (default 4.0) mapped to γ per preset; validation rejects negatives. (3) Solver grain: `relieve_over_capacity` pre-splits every over-capacity real-folder cluster along positively-priced connectivity between its SCCs into cap-respecting halves — pile 0 keeps the original cluster id and real directory name, later piles get fresh clusters named `{dir}-{stem}` from their dominant basename stem (numeric fallback), and split files' home domain keys are rewritten after the identity clone so coarsening cannot weld halves back together
- **Reason**: behavior-change (measured defect: relieving an over-cap folder earned exactly zero score — five-term breakdowns stayed byte-identical across cap-changing probes — so nothing ever asked candidates to relieve; the relief eval witness pinned this)
- **Impact on spec**: behavior-change — scores, standings, and best candidates shift wherever a scoped container breaches its budget; ScoreBreakdown DTO gains a sixth `capacity` field (JSON `capacity`, schema v2 additive)
- **QUAL-P3-2 bundled**: `walk_capacity` upper levels now measure real binding members deeply instead of direct children only — Domain counts descendant file-binding folders, Package descendant binding domains, PackageGroup descendant binding packages; interior directory chains count once (only folders directly holding files), so nesting can neither launder binding out of a finding nor price one chain segment per path level
- **Known residual (measured, not fixed)**: on the relief fixture the relieved halves still bleed their chain-head SCCs ({00,01} of each 12-chain) into the root/main cluster during polish. Attribution: each accept trades one crossing for another (cut identical to 17 digits), γ·pressure inert (1.0→1.0 — every container within budget), anchor inert; the entire gain is `imbalance` −0.0096/file. The relief witness therefore stays red on its `keep_together` pins while `capacity_relief`, `separate`, and `size_band` pass. Fixing requires either pricing home-coherence loss (the dead β/path term) or an imbalance-semantics change — both out of FIX03 scope, routed to the ladder
- **Severity**: major
- **Update (2026-08-29)**: The recursive/transitive Folder measure is superseded by immediate physical entries (immediate files plus distinct immediate child folders). Capacity remains in the objective with the same coefficient; non-Folder measures are unchanged.

### D-45: The "breaks N capacity findings" count excludes borderline severity; the engine owns the count
- **When**: WS-D defect burn-down FIX10 (2026-08-23)
- **Draft said**: nothing — the DSG04 renderer tallied its own section-4 capacity list for the infeasibility note, counting borderline-severity findings as breaks even though a borderline observation sits within tolerance and never gates an `infeasible` standing (the engine's own `capacity_clean` predicate counts `Severity::Violation` only); on mixed fixtures the note overstated breaks while the markdown face counted correctly
- **What I did instead**: the authoritative count moved into engine semantics — `CurrentTree.capacity_breaks: u32` (additive DTO field), computed by one `hard_capacity_breaks` predicate shared with `capacity_clean`; both narration faces consume it and the renderer-local `hard_capacity_count` helper was deleted so no face can re-tally divergently. Section 4 remains complete: every finding including borderline stays listed with its severity tag; only the count changes
- **Reason**: behavior-change per owner ruling 2026-08-23 (two faces of the same facts disagreed; the count must match what actually gates)
- **Impact on spec**: behavior-change — the infeasibility note now reports hard breaches only; additive JSON field `capacityBreaks`
- **Severity**: minor

### D-46: Zero-priced edges bind nothing — they neither nominate move targets nor contract vertices
- **When**: WS-D defect burn-down FIX04 (2026-08-23)
- **Draft said**: nothing — the spec is silent on whether a free edge may influence placement; the search admitted every edge at its configured price and then let zero prices participate: `pull_targets` accumulated 0.0-pull folder nominations ranked into polish's top-4 move targets (every cross-directory nomination on the welding barrel fixture came from w=0 re-export edges), and heavy-edge matching's first-candidate default contracted across zero-priced edges on graphs above the seed threshold
- **What I did instead**: the FIX04 doctrine already present in `connected_piles` became universal. (1) Engine `pull_targets` skips edges priced ≤ 0 before accumulating pull, so a folder connected only through free edges is never offered as a move target. (2) Core `pick_partner` skips partners across edges priced ≤ 0, so matching never contracts two vertices a free edge happens to touch. (3) Edges still enter the file graph at their configured price (unchanged — AD-6 alignment of search cut with reported cut survives; soft-only files keep their mobility)
- **Reason**: behavior-change (a zero weight means "no evidence these files belong together", yet it could weld them into one folder; the welding eval target pins exactly this)
- **Impact on spec**: behavior-change — candidates can differ wherever a zero-priced edge (default `re_export = 0.0`) previously nominated a polish move or joined a matching pair; no committed fixture or golden moved under this change (all parity goldens and every holds-best-state eval reproduced byte-identically)
- **Severity**: minor

### D-47: An unchanged layout is reported faithfully, not re-assembled; polish may neither absorb a multi-folder bridge nor tear measured company
- **When**: WS-D defect burn-down FIX05 (2026-08-23)
- **Draft said**: nothing — the spec never states how a candidate whose file partition equals the real-directory layout is rendered, nor whether polish may relocate an SCC away from the folder holding its priced evidence. The engine answered both by accident: `build_candidate` keyed its faithful "change nothing" exit on the anchored-only `identity` field, so a greenfield candidate byte-equal to reality was routed through `assemble()`, which re-elects upper-level containers and nests them differently from the current tree — "change nothing" rendered as structural movement for every file whose fabricated chain differed, and greenfield was billed for movement it never proposed (`inversion_witnesses_anchored_inversion`). Separately, polish's acceptance test priced only the destination: absorbing a facade that imports three features into one of them read locally cheap because the adopted edges dropped to folder height while the abandoned ones kept theirs — so the unbiased mode out-churned anchored, inverting AD-2's product promise
- **What I did instead**: three structural changes at the two defect sites. (1) `PipelineSolver` records `real_is_identity`; `build_candidate`'s faithful exit now also fires when the solved partition equals the real-directory partition and relief made no split — mode-independent measurement fidelity: the same physical layout gets the same truthful tree and score in both modes. When relief did split an over-capacity folder the search start is no longer reality, so assembly stays (the split is what the proposal must show). (2) Two polish vetoes before evaluation, both zero-priced-transparent per D-46: bridge integrity (`absorbs_a_foreign_anchor` — an SCC with a priced anchor outside {source, target} is orchestrating those folders and may not be absorbed into one of them) and cohesion integrity (`strands_a_comparable_anchor` — relocation requires the destination to out-pull what would be stranded; ties stay). A third veto closed the escape the first two created: feature members fleeing their real directories INTO the synthetic `workspace` bucket (`flees_into_the_synthetic_bucket`) — the bucket is the absence of a place, so a file with priced company in its own folder may not relocate into it at all
- **Reason**: behavior-change (the eval corpus pins both directions: `inversion.toml` budgets anchored AND greenfield at zero moves on a facade fixture; `tearing.toml` states the doctrine verbatim — "absorbing it into one feature makes the bridge a member of the thing it bridges"; `collapse.toml` forbids the invented bucket swallowing real directories)
- **Impact on spec**: behavior-change — candidates differ wherever polish previously absorbed bridges, tore comparable anchors, or flew into the synthetic bucket, and unchanged layouts are no longer re-assembled. Eval witness set moved 4 → 1 (`inversion_`, `tearing_`, `relief_` hold their best state; `naming_drift_` remains open). Disposition COMPLETED 2026-08-23 (PM-reviewed, same day): premise-death verified on all nine engine tests before any retune (each failing fixture emitted exactly one faithful identity-tree candidate per mode); every fixture retuned so one genuine sole-anchor satellite consolidation occurs under the vetoes, preserving each original naming guard verbatim; four acceptance checks re-pinned to PM-verified post-fix shapes (dual-face fabrication guard on workspace-ts-leak, move-narration re-pointed to constellation-ts greenfield/1, depth truncation against real package lines, relative-root pinned by byte-equal path-form independence). Eleven parity goldens regenerated after per-hunk diff review attributing every movement to the fidelity gate (unchanged layouts honestly read `same shape, own baseline`; phantom-move blocks became truthful no-move narration; constellation-ts greenfield's twelve-file multi-folder sweep became five sole-anchor moves) and one anchored face gained two genuine priced pulls (+0.0256) the old mispricing drowned. Gate restored at 623/624, failures exactly `naming_drift_witnesses_naming_incoherence`. The open product question "is a domain a directory or a view?" narrows to genuinely-moved candidates only and rides with FIX09
- **Severity**: major

### D-48: The AD-2 seeding gate was implemented but unproven; verified against its documentation and pinned, no behavior change
- **When**: WS-D defect burn-down FIX06 (2026-08-23)
- **Draft said**: the gate might only be described — `analyze_inner`'s comment claims "identity seeding is anchored-only (AD-2) and requires a cap-clean current tree", and nothing in the suite exercised that claim
- **What I did instead**: verified every documented half end to end and pinned them without changing behavior. (1) Wiring: anchored receives `seed_identity = capacity_clean`, greenfield hard `false`. (2) Mechanism: `PipelineSolver` constructs `identity` only from the flag; the offset-0 seed short-circuits to the identity entry re-priced by `score_current`; `finish` re-prices only identity-matching partitions; `diversify` seeds `base_seed + i` from i = 0, so the offset-0 arm always engages. (3) Coefficients: `ObjectiveConfig::greenfield()` forces β = μ = 0 regardless of configured values while γ stays shared (already pinned by `should_zero_path_and_anchor_in_greenfield_coefficients`). Three engine tests now hold the gate: standings asymmetry on a cap-clean edgeless fixture (anchored optimal with a zero-move candidate at +0 improvement; greenfield outscored yet reporting candidates), flag-response at the solver level including the true-current re-price and full SCC coverage when closed, and both-modes-infeasible on a hard breach with borderline findings excluded by the same `hard_capacity_breaks` predicate the DTO reports as `capacity_breaks`. Mutation checks confirmed the pins bite in both directions (opening greenfield's arm or closing anchored's flips the standing and fails the test). Recorded consequences, not defects: a greenfield candidate equal to reality can still appear, but only through search convergence rendered faithfully by the FIX05 `real_is_identity` exit — found, never seeded; and because `optimal` is defined as "the identity entry won the pool", greenfield's standing is structurally unable to reach it
- **Reason**: verification (the suspicion was wrong in direction — the gate exists and matches its documentation — but right in substance: nothing would have caught its removal)
- **Impact on spec**: none (test-only change; no production code, DTO, golden, or eval-witness surface moved)
- **Severity**: minor



### D-49: The naming-drift witness's anchored reds are honorable scoring; the target pins measured behavior instead of waiting on a scorer change
- **When**: WS-D defect burn-down FIX09 disposition (2026-08-24)
- **Draft said**: the anchored face of `naming_drift_witnesses_naming_incoherence` was expected to regroup the misnamed `helpers` roof exactly like greenfield does, so its two red verdicts (`name_alignment` 0/5 and `separate(string_utils.py+charge.py)`) read as defects still to fix in the engine
- **What I did instead**: measured the objective end to end and pinned what it actually elects. At live defaults (anchored λ=.1 α=.3 β=.2 μ=1 γ=4; greenfield zeroes β+μ only) the anchored pool at tolerance filters both rivals of the winner — identity (+0.3626) and the split (+0.9771, the exact shape greenfield elects) — so candidate 1 is the weld-plus-absorb (J=+0.0979; cut .125, imbalance .020, naming −.010, path −.180, anchor +.1429): all five files incl. `checkout.py` legally absorbed into folder `helpers` as a sole-anchor satellite (cut .5000→.1250 dominates). The split loses honorably: it pays an anchoring term of .8571 vs the weld's .1429 (6/7 symbols displaced across the priced `checkout→charge` edge), a total deficit of 0.879 ≈ anchor delta 0.714 + path-bonus delta 0.16 − naming gain 0.04 (α·naming prices symbol-token cohesion inside containers, never folder-name-vs-content alignment). μ sweep with β pinned at .2: the weld wins at every μ incl. 0; only β=0 AND μ≈0 (= greenfield's coefficients) flips it, and then by just 0.0107. No coefficient value changed (hard law). Per owner ruling the target was retargeted instead of the scorer: `naming-drift.toml` keeps its single required `[[assert]]` group but pins face-specific assertions by their own `mode` override — `name_alignment`, `separate`, and one payment-pair `keep_together` greenfield-only where they pass today; `preserve_dir("helpers")` and a second payment-pair `keep_together` anchored-only, each because citing these measured numbers — leaving the FIX08 `preserve_symbol_home` pins on their original both-face scope untouched. The stale module doc atop `crates/eval/tests/eval.rs` claiming a four-witness red set was trued to the terminal zero-red state
- **Reason**: verification per owner ruling 2026-08-24 (the disagreement between the defended best state and the scorer is the finding, and the corpus records it rather than tuning either side; editing the target file was sanctioned for exactly this purpose, superseding the older no-target-edits constraint)
- **Impact on spec**: surface-change — eval-target assertion faces reorganized; no production-code, DTO, golden, or coefficient movement (the witness verdicts move from red-by-design to pinned-green)
- **Severity**: minor

### D-50: Cycle-span's tearing demand is unsatisfiable under current law; the owner ruled the enabling partitioner redesign a follow-up stream
- **When**: WS-D FIX11 escalation + owner ruling (2026-08-24)
- **Draft said**: FIX11 (registered that morning as "fixed NOW") assumed the cycle-span tear was a seed-instant artifact — a mixed-home SCC fold tearing `shaft.py` out of `drivetrain` at seed granularity — removable by a home-preserving fold target or a FIX05-family structural veto
- **What I did instead**: measured both paths to exhaustion and escalated. The fixture's real layout itself splits an import-cycle atom across directories (`engine/motor.py` ↔ `drivetrain/shaft.py` import each other; reality holds motor+ignition under `engine/`, shaft+coupling under `drivetrain/`). SCC atoms are inseparable at partition grain, so EVERY lawful candidate fails a `preserve_dir` on one face or the other — satisfying both pins requires reproducing reality's split, i.e. splitting the atom. A fold-target flip only mirrors the tear (start-insensitive, J=0.1205 either way); barring tear-shaped candidates empties the pool rather than electing a survivor; and end-to-end the objective honors the tear it is handed (elected cut .125 vs ≈.40625 for the unreachable reality shape). The seed-instant premise of the morning ruling is falsified — the tear is the objective's election, not an artifact (the same honorable-scoring shape as D-49, on the cut term). A round-2 structural-veto prototype landed mid-flight with a worker-reported zero-red measurement (UNVERIFIED — its session died before reporting gates); it was REVERTED per owner ruling, diff preserved at `/tmp/pp-fix11-veto-evidence/veto-vs-pristine.patch` (volatile — rehome into the follow-up stream if wanted). Per structured question answered 2026-08-24T20:08Z the owner chose the Architectural path: change the partitioner so multi-file SCC atoms CAN split across directories and proposals can reproduce reality's split layout. Until that follow-up stream landed, `cycle_span_witnesses_import_cycle_tearing` stayed red by design as the standing witness for the gap, and the eval module doc recorded the one waived red
- **Reason**: owner ruling [decisions/cycle-span-partitioner-redesign.md] — pinning rejected, ship-with-documented-red rejected; coefficient hard law untouched; edge admission unchanged; FIX11 closed by amended acceptance (in-stream closure = this entry + doc truthing + backlog row)
- **Impact on spec**: none in-stream — no production code, TOML, DTO, golden, or coefficient movement; the witness moves from red-pending-fix to standing-red-by-ruling; the redesign stream inherits the measured chain above plus the preserved veto evidence
- **Severity**: minor
- **Resolution (2026-09-25)**: at `c18f840` the witness passes (`cargo nextest run -p strata-eval`: 36/36 green), so the standing red is retired; the witness now guards against import-cycle tearing regressions and the eval module doc no longer records a waived red
