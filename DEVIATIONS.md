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
