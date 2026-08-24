# FIX07 — SCC seed-granularity probe: polish-vs-seed split

Ladder item FIX07 of the WS-D defect burn-down. Hypothesis under test:

> Mixed-home SCCs tear at SEED time, before the cohesion/naming term J is ever
> consulted, so a large share of final-structure error originates at seed
> granularity rather than in polish.

Instrument: `crates/eval/examples/fix07_scc_split.rs` (new, read-only over
public APIs; production behavior untouched and byte-identical). Measured across
all seven fixtures in `crates/eval/fixtures`.

**Verdict: AGAINST on this corpus.** Of 13 displaced files corpus-wide,
12 are deliberate FIX03 relief moves (J-ratified post-relief) and 1 is a polish
move; 0 are seed-granularity tears. The mechanism the hypothesis names never
engages because the corpus contains zero import cycles, hence zero multi-file
SCCs to fold. This is a coverage gap, not a refutation of the mechanism — see
"Follow-on".

## 1. Operationalization

The definitions below are the measurement contract for this probe. They are
recorded here explicitly because the split is an attribution judgment, and any
other split must state its own.

### Search grain

`PipelineSolver::new` builds a priced file graph (every edge at
`weights.edge_weight(kind, confidence)`), condenses it with Tarjan
(`condense()`), and every partition node IS one SCC. Polish's operator space
(`Partition::move_node`), relief, and MFAS break advice all act on whole SCCs.
Nothing splits an SCC before render.

### The seed decision

`real_dir_partition` folds each SCC onto its **dominant member's home
directory** (max production SLOC; lexicographic name tiebreak). For a mixed-home
SCC this is the instant individual placement is sealed: the fold translates the
whole SCC and cannot restore a member that belongs elsewhere.

### Final structure

The anchored candidates[0] (lowest J) from `analyze()`. Delta narration
(`delta_narration`) is the authoritative complete folder-diff versus current.
Greenfield candidates are excluded by design (AD-2: greenfield must be
layout-blind), so all attribution is against anchored structures only.

Folder-level layout = identity partition + FIX03 relief splits;
`cluster_level`/seed/refine run only at upper levels inside `assemble()`.

### Displaced file

A file whose rendered path differs between current layout and final structure,
i.e. it appears in the best candidate's `delta_narration`.

### Attribution precedence

Each displaced file gets exactly one origin, resolved in this order:

1. **Relief** — its move reason is `MoveReason::RelievesOverCap`. Relief runs at
   seed-stage capacity construction but is J-ratified afterward (candidates
   compete post-relief), so relief displacement counts separately from both
   seed error and polish error.
2. **SeedGranularity** — otherwise, if the file's replica SCC is mixed-home
   (spans >= 2 real directories). Its placement was sealed by the dominant-home
   fold at seed time; whole-SCC translation could not have repaired it
   individually, so the displacement is seed-attributable regardless of which
   pull moved the SCC.
3. **Polish** — otherwise: a single-file SCC displaced by a whole-atom move that
   polish chose.

### Structural floor metric

`seed_torn_struct` = sum over mixed-home SCCs of members outside their SCC's
dominant directory. This is displacement created AT the seed instant, before J
scores anything, independent of whether translation later lands the SCC well.
It bounds how much error seed granularity can cause even when translation is
perfect.

## 2. Instrumentation design

A single example binary, `crates/eval/examples/fix07_scc_split.rs`, mirroring
the harness invocation per fixture (`load_target` + same `AnalyzeConfig`
construction as `run_case`, then `snapshot_from_root` + `analyze`). It reads
only public APIs; no production code path changes, so non-enabled behavior is
byte-identical by construction (the instrument does not exist in production).

Hooks:

- **Replica condensation** — rebuilds the priced file graph via public core APIs
  (`Csr::from_weighted_edges`, `condense`) and derives per-file SCC membership.
- **Narration diff** — flattens the best candidate's `delta_narration` into
  displaced files and applies the precedence above.
- **Validations** — V0: IR-derived file paths equal DTO-rendered file paths as
  sets ("path identity verified"); V1: narration move groups are SCC-atomic (no
  partial groups observed anywhere); V2: engine-reported cycle violations equal
  the replica's multi-file SCC count.

## 3. Per-fixture results

```
fixture      files multi mixed files_in_mixed torn disp rel mix pol moves
collapse         5     0     0              0    0    0   0   0   0     0
large-app      108     0     0              0    0    0   0   0   0     0
welding          5     0     0              0    0    0   0   0   0     0
tearing          6     0     0              0    0    0   0   0   0     0
relief          25     0     0              0    0   12  12   0   0     1
inversion        7     0     0              0    0    0   0   0   0     0
naming-drift     5     0     0              0    0    1   0   0   1     1
TOTAL          161     0     0              0    0   13  12   0   1     2
```

Columns: multi/mixed = multi-file / mixed-home SCCs; torn = `seed_torn_struct`;
disp = total displaced files split into relief/seed-mixed/polish; moves =
narration move groups in the best candidate.

Cross-checks, every fixture:
`engine_cycle_findings=0 best_conditional_splits=0 replica_multi_file_sccs=0
pool_converged=true`; V0 path identity verified everywhere; V1 no partial SCC
groups.

Standings: Optimal (collapse 0.086, large-app 0.527, welding -0.023,
tearing 0.071, inversion 0.285); Infeasible (relief 1.454, improvement +0.137);
Outscored (naming-drift 0.098, improvement +0.265).

Displacement detail:

- relief: `hub/ingest_00..11.py` out of `relief/hub` — all Origin::Relief
  (`RelievesOverCap`), matching the fixture's designed witness.
- naming-drift: `checkout.py` out of `naming-drift` — Origin::Polish, matching
  the fixture's designed witness.

## 4. Verdict

**AGAINST the hypothesis on this corpus — with the honest caveat that the
corpus cannot test it.**

- Seed-attributable displacement (SeedGranularity): **0 / 13**.
- Polish-attributable: 1 / 13 (naming-drift checkout.py).
- Deliberate, J-ratified relief: 12 / 13 (relief fixture).
- Structural floor `seed_torn_struct`: **0 corpus-wide**.
- Multi-file SCCs: **0 corpus-wide** (161 files). Zero dependency cycles means
  Tarjan produces only singletons, so the dominant-home fold degenerates to
  identity and the hypothesized tearing mechanism has nothing to tear.

So: where the mechanism exists, we measured zero contribution; but the eval
fixtures contain no cycle-bearing code, meaning the hypothesis is effectively
untestable on this corpus. The correct reading is "unsupported *and* uncovered,"
not "refuted." No production defect surfaced — relief and naming-drift behave
exactly as their witnesses are designed to expect.

## Follow-on (new ladder item, NOT done here)

Add one cycle-bearing fixture (a genuine import cycle spanning two real
directories) so the mixed-home SCC path actually executes. Then re-run this
probe unchanged: `mixed_sccs`, `files_in_mixed`, `seed_torn_struct`, and
`disp_mix` will become nonzero and the polish-vs-seed split becomes measurable.
The instrument is already in place and costs no production change.

---

## Addendum (2026-08-24, FIX09): the follow-on executed on fixture `cycle-span`

Appended by the FIX09 work stream; everything above is unchanged. The follow-on
fixture now exists: `crates/eval/fixtures/cycle-span/` (5 Python files) whose
priced file graph carries exactly one multi-file SCC spanning two real
directories — `engine/motor.py <-> drivetrain/shaft.py`, both legs genuine Call
edges at confidence 1.00 / price 1.000 / hardness Hard, proven in the priced
graph BEFORE any harness assertion was authored.

### Probe row (instrument unchanged, `FIXTURES` extended to 8)

```
fixture      files multi mixed files_in_mixed torn disp rel mix pol moves
cycle-span       5     1     1              2    1    0   0   0   0     0
TOTAL          166     1     1              2    1   13  12   0   1     2
```

Every legacy row of section 3 reproduced byte-identically; only the totals grew.

### What the mechanism does now that it has something to tear

- **Seed instant** — the mixed-home SCC `{motor, shaft}` folds onto its
  dominant member's home directory `engine` (motor carries more SLOC);
  `drivetrain/shaft.py` is outside its SCC's dominant directory at that
  instant, so `seed_torn_struct = 1`. This is the first nonzero structural
  floor in the corpus: the hypothesized tearing mechanism ENGAGES.
- **Anchored final structure** — the tear never reaches it. Anchored candidate
  1 scores 0.234 with improvement 0.000 and zero move groups
  (`pool_converged=true`): the identity entry outscores every rebuild lineage
  carrying the fold, so displacement attribution stays Relief/Seed/Polish =
  0/0/0. J rejects the seed's tear at pool-selection time; the floor metric
  records damage the witnessed structure does not carry.
- **Greenfield face** — excluded from this instrument by design (AD-2), but
  measured separately during FIX09: greenfield welds `shaft.py` out of
  `drivetrain` into an `engine` container (best-state J = +0.1658). That
  distance is now pinned red-as-designed by the eval target
  `crates/eval/targets/cycle-span.toml`
  (`preserve_dir("drivetrain")` failing on the greenfield face while anchored
  passes everything at zero churn).

### Verdict on the new fixture

CONFIRMED, with one refinement: mixed-home SCCs do tear at seed time exactly as
hypothesized (`mixed_sccs=1`, `files_in_mixed=2`, `seed_torn_struct=1` — all
were 0 corpus-wide before this fixture), and the polish-vs-seed split is now
measurable. The refinement: on a layout that is otherwise healthy, anchored J
recovers the tear through pool selection (identity wins), so seed-granularity
error surfaces in the FINAL structure only where no identity escape hatch
exists — the greenfield face, which AD-2 places outside this instrument's
attribution. Seed tearing remains real; its witness lives in the greenfield
eval verdict, not in anchored displacement counts.

### Crosscheck caveat (V2 divergence, recorded as a finding)

On this fixture the engine reports `engine_cycle_findings=0` while the replica
finds 1 multi-file SCC. Both are correct: the engine's Cycle violations come
from condensing the SYMBOL-level hard-edge graph (`solve_cycles` over IR nodes,
whose projection here is the DAG run_motor -> spin -> torque_curve), while the
search grain that tears is the FILE-level graph. Strata therefore reports
cycles at symbol granularity but tears at file granularity at seed time; a
file-only import cycle is invisible to the violation layer. For this reason the
`cycle-span` target deliberately omits any `violation_present(cycle)`
precondition — such a precondition could never pass and would misreport the
fixture as defective.
