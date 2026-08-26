# Eval corpus contract — fixtures, targets, evaluation

Authoritative contract for the synthetic eval corpus (`crates/eval/fixtures/`,
`crates/eval/targets/`). The eval harness (WS-C, `crates/eval/src`) implements
this document; fixture and target authors must conform to it. Contract schema
version: **1** (the `schema` key in every target TOML).

## Purpose

Measure distance-to-best-state: run strata on a synthetic repo that is
deliberately unsatisfying in exactly one way, then check the proposal against a
machine-checkable constraint target encoding what a *good* proposal must do.
Assertion failures are signal, not shame — they are the gate every algorithm
fix must move toward zero. Targets are written from the best state we can
defend, never tuned to make today's binary pass. When the scorer disagrees
with the defended best state, the target stands and the disagreement is the
finding.

## Fixture rules

1. **Synthetic only.** No real OSS code, no copied prose, no generated-at-runtime
   content. Every byte is committed and deterministic (no timestamps, no
   randomness, no host-dependent paths).
2. **One failure mode per fixture**, named by `[meta].failure_mode` from the
   canonical list below.
3. **Adapter-parseable.** `strata analyze --root <fixture> --config
   /nonexistent.toml --format json` must exit 0 with no syntax-error findings.
4. **No `Cargo.toml` inside any fixture.** A manifest would drag the fixture into
   cargo's workspace resolution; Rust-flavored scenarios use the TS multi-package
   form instead until a fixture workspace is excluded like the e2e one.
5. **Style** follows `crates/cli/tests/e2e/fixtures/`: tiny files (roughly 4–25
   lines), docstrings naming intent, imports expressing exactly the intended
   dependency graph, nothing else. Python needs no manifest; TS packages mark
   their root with a minimal `package.json` only when package-boundary semantics
   matter.
6. A fixture may carry its own `strata.toml` to shape caps; then `[run].config =
   "fixture"`. Default is pure built-in defaults (`[run].config = "defaults"`),
   which is what all first-batch fixtures use so results stay comparable.

Canonical failure modes: `directory-tearing`, `cross-dir-welding`,
`over-capacity`, `workspace-collapse`, `anchored-inversion`,
`naming-incoherence`, `import-cycle`, `scale`. The `import-cycle` mode covers
fixtures whose designed defect is one genuine priced import cycle spanning two
real directories — the substrate the FIX07 seed-granularity probe measures.

## The evaluation surface

All assertions read one JSON document:

```
strata analyze \
  --root <repo>/crates/eval/fixtures/<name> \
  --config /nonexistent/strata-eval.toml        # when [run].config = "defaults"
  --mode <[run].mode> --seed <[run].seed> -k <[run].candidates> \
  --format json
```

`config = "defaults"` is implemented by pointing `--config` at a missing file:
the binary warns on stderr, uses built-in defaults, and exits 0. When the
harness grows a runner, pin that warning-and-defaults behavior with a
precondition or add an explicit defaults flag.

That JSON is the engine's `AnalyzeResult` DTO (`crates/engine/src/result.rs`,
camelCase). The harness evaluates preconditions against `current`, assertions
against `modes.<mode>.candidates[<candidate>-1]` for each mode listed under
`[assert]` (unless an assertion names its own mode), and pair-F1 against the
optional reference tree. A precondition failure means the harness or fixture is
broken — report it as an error, never as distance.

Naming facts the harness relies on (from `crates/engine/src/analyze.rs` and
`crates/ir/src/laminar.rs`):

- File nodes keep their **full repo-relative path** as `name`, in current trees
  and candidate trees alike, so file identity is stable across trees.
- Non-file container names are **full-prefix keys relative to their package
  root** (`store`, `ai/adapters`): folders keep real directory names, upper
  levels are elected from member-file votes, and the fallback name for an
  unresolvable group is literally `workspace`. A top-level directory under a
  package is therefore just its basename.
- Source roots (`src`, `spec`, `tests`, …) are transparent: below a package
  root, one leading source-root segment is stripped, so `atlas/src/store/kit.ts`
  lives in container keys `store/*` under package `atlas`. Target container
  paths are written in this post-strip, package-relative form; target FILE
  paths are always full repo-relative paths.
- Node identity is the root-path of (level, name) pairs, never a name alone:
  rendered trees mint same-named nodes at two levels (`domain:billing` and
  `folder:billing` share one rendered name), and a level's presence varies
  between current and candidate trees. Never deduplicate by name and never
  assume a level exists in a given tree.

Membership of a non-file node `v` — written `members(v)` — is the set of
`name`s of its DIRECT file children only. Counting is first-level: a folder
holds exactly what sits in it, and nested sub-containers contribute nothing to
an ancestor's membership, because their files already belong to their own
place. Splitting an over-cap folder into halves that nest under it therefore
reads as two within-band places, not one still-over-cap umbrella. (The
transitive descendant set remains the census form — universes and placement
initialization — but no structural predicate counts with it.)

### Container scope principle (root-ward envelopes are exempt)

Package and packageGroup nodes are envelopes: in a single-package repo every
file trivially shares the package, so pairwise, band, and alignment assertions
evaluated there can only restate the file census. Therefore `separate`,
`keep_together`, `size_band` with `scope = "any_container"`, and
`name_alignment` evaluate **only non-file nodes at level `folder` or
`domain`**. A file sitting loose directly under a package has no folder/domain
container, so pairwise assertions never fire on it — dissolution of a real
directory is `preserve_dir`'s signal, co-location is `separate`'s, and the two
never double-count. (Each rule was originally "every non-file node";
hand-validation showed that conflates envelopes with structure. The division of
labor above is the fix, not a weakening: every defect still has exactly one
assertion kind that owns it.)

Tokenization (for `name_alignment`): lowercase; split on `/`, `_`, `-`, `.`
and camelCase humps; drop empty and all-digit tokens. A hump splits before an
uppercase letter that follows a lowercase letter or digit; an acronym run
splits as one token (`JSONBlob` → `jsonblob`). Container tokens come from the
**last segment** of its full-prefix name; file tokens from the basename minus
extension.

## Target TOML schema

Every target is `crates/eval/targets/<name>.toml`, `name` equal to its fixture
directory. Unknown keys are a harness error (fail loud, never guess). Every
`[[precondition]]` and `[[assert.*]]` block carries a non-empty `because`
comment-rationale string: the human-defensible reason the constraint holds in
the best state; `[meta]`, `[run]`, and `[[reference.container]]` blocks omit
it. Rationale-free preconditions or assertions are invalid.

```toml
schema = 1                      # contract version, must be 1
fixture = "<name>"              # must match the fixture directory

[meta]
failure_mode = "<canonical>"    # see list above
language = "python|typescript"
question = "<the single question this fixture asks>"

[run]                           # the invocation every assertion was validated against
mode = "both"                   # analyze --mode: anchored | greenfield | both
candidates = 3                  # -k
seed = 42                       # --seed
config = "defaults"             # defaults | fixture

# --- preconditions: verified against result.current before scoring -------------
# kind = "file_count": summary.files within [min, max]
[[precondition]]
kind = "file_count"
min = 6
max = 6
because = "the fixture ships exactly six files; any other count means discovery broke"

# kind = "violation_present" / "violation_absent":
#   violation = cycle | polarity | capacity | visibility  (matches ViolationKind)
#   location_suffix = optional; matches on DOT-SEGMENT boundaries only — the
#     location's last dot-segment equals it, or the location ends with
#     ".<suffix>" (so `hub` matches `hub` and `a.hub`, never `xhub`)
#   Severity: findings of ANY severity satisfy the precondition unless the
#     target opts out via an explicit `severity` key (then it filters exactly)
[[precondition]]
kind = "violation_present"
violation = "capacity"
location_suffix = "hub"
because = "hub holds 24 files against the default folder cap of 20"

# --- assertions: evaluated per mode on modes.<m>.candidates[candidate-1] -------
[assert]
modes = ["anchored", "greenfield"]   # modes whose best candidate must satisfy them
candidate = 1                        # 1-based; 1 = best-scoring

# The directory survives as a named container. `path` is a container key in
# the post-strip, package-relative form above, while fixture files are
# repo-relative — so membership resolves PER PACKAGE: for each analyzed
# package P, let D(P, path) be the fixture files whose repo-relative path
# lies under `<P.root>/[<source-root>/]<path>/`. For every P where
# D(P, path) is non-empty, some non-file node at level folder or domain,
# named exactly `path`, whose position within P contains all of D(P, path),
# must exist — the directory survives once per package that physically has
# it. (Without this resolution rule the file set is empty on every fixture
# whose directory sits under a source root or package root, and any node
# bearing the name passes with arbitrary members.) Files welding ONTO the
# directory are not this assertion's concern; `separate` and `size_band`
# own that defect. (Exact-membership was tried and rejected during
# hand-validation: it conflates "directory torn" with "something welded in",
# which muddies per-fix attribution.)
[[assert.preserve_dir]]
path = "billing"
because = "..."

# some folder/domain-level node's members include every listed path
# (co-location required; see Container scope principle)
[[assert.keep_together]]
paths = ["a.py", "b.py"]
because = "..."

# no folder/domain-level node's DIRECT file children include two or more of
# the listed paths (must-not-co-locate; also the pairwise unit of pair-F1;
# first-level membership — nesting one place under another never reads as the
# two places merging)
[[assert.separate]]
paths = ["a.py", "b.py"]
because = "..."

# size band on first-level members only:
#   scope = "any_container"            → every non-file node at level `folder`
#                                        or `domain` (package/packageGroup are
#                                        exempt: they legitimately hold whole
#                                        packages, so a global band there can
#                                        only ever restate the file census),
#                                        OR
#   container = "<full-prefix name>"   → each node with that name, any level
# A folder's count is its direct file children; nested sub-places contribute
# nothing, so halves that path-extend their base folder each measure on their
# own and the band binds exactly where the cap binds in the engine.
[[assert.size_band]]
scope = "any_container"
max_files = 20
because = "..."

# bounds the amount of change one mode proposes (overrides [assert].modes).
# Moved files are counted STRUCTURALLY, never from deltaNarration: a moved
# file is one whose folder/domain-level container path differs between the
# current tree and the candidate tree (a loose file has the empty container
# path). Narration is a lossy proxy — placement changes have been observed
# with empty narration — so it never feeds scoring; tree/narration divergence
# is tracked separately as a finding. Bounds are inclusive.
[[assert.move_budget]]
mode = "anchored"
max_moved_files = 0               # min_moved_files also available
because = "..."

# no non-file node whose last name segment equals `name` (catches the synthetic
# `workspace` bucket and any descendant-named variant). This means "no
# container is *called* workspace", so fixtures must never ship a real
# directory named `workspace` — rename in fixtures if one is ever needed.
[[assert.no_synthetic_bucket]]
name = "workspace"
because = "..."

# every non-file node AT LEVEL FOLDER OR DOMAIN holding >= min_members files
# keeps at least min_ratio of them sharing >=1 token with the container's own
# tokens (see Tokenization). Package/packageGroup envelopes are exempt per the
# scope principle: manifest- or root-derived package names never align with
# member basenames, so including them makes the rule unsatisfiable by
# construction.
[[assert.name_alignment]]
min_ratio = 0.5
min_members = 2
because = "..."

# the best candidate leaves zero hard capacity findings:
# capacityRemainder absent, or capacityRemainder.remaining == 0
[[assert.capacity_relief]]
mode = "greenfield"
because = "..."

# anchored must not move more files than greenfield (needs run.mode=both);
# moved files counted structurally as for move_budget
[[assert.non_inversion]]
because = "..."

# a SYMBOL keeps its current home file in the asserted candidate. `path` is
# the symbol's FULL repo-relative file path in today's layout (not the
# package-relative container key preserve_dir uses); the harness checks at
# load that the CURRENT tree really shows the symbol in that file, so the pin
# documents an existing home rather than wishing one into existence. The best
# candidate of each asserted mode must still show the symbol in a FILE node
# with exactly that path — folder moves above the file are fine, moving the
# symbol out is not. Use it to pin that a defect lives in the misleading
# ROOF (naming/placement of containers), not in member placement, so fixing
# the roof must not churn innocent symbols.
[[assert.preserve_symbol_home]]
symbol = "charge_card"
path = "src/billing/helpers/charge.py"
because = "..."

# --- optional reference best state: pair-F1 ONLY, never pass/fail ---------------
# `path` is a label only: pair-F1 reads the listed files, never the name.
# Reference names are NOT naming obligations (an ideal roof like `payments`
# would itself fail name_alignment; member-elected roofs are what count).
[[reference.container]]
path = "billing"
files = ["billing/invoice.py", "billing/pricing.py", "billing/ledger.py"]
```

At load the harness validates every referenced file path against the fixture
census, every `preserve_dir` key against the package/source-root layout, and
every `preserve_symbol_home` pin against the CURRENT tree's symbol homes. A
misspelled path is a load-time error, never a verdict — a silent typo would
mint permanent fake distance, which is the corpus's core currency.

Per-kind keys (missing-required and unknown alike fail loud):

| kind | required | optional |
| --- | --- | --- |
| preserve_dir | `path` | `mode` |
| preserve_symbol_home | `symbol`, `path` | `mode` |
| keep_together / separate | `paths` (≥2) | `mode` |
| size_band | `max_files`; exactly one of `scope`/`container` | `min_files`, `mode` |
| move_budget | `mode`; exactly one of `max_moved_files`/`min_moved_files` | — |
| no_synthetic_bucket | `name` | `mode` |
| name_alignment | `min_ratio`, `min_members` | `mode` |
| capacity_relief | `mode` | — |
| non_inversion | — | — |

An assertion's own `mode` overrides `[assert].modes` for that assertion only.

### Pair-F1

From the reference containers build the set `R` of unordered co-membership
pairs over exactly the listed files. From each asserted candidate tree build
the same set `P` over those same files using `members(v)` of folder/domain
nodes only — package/packageGroup envelopes are excluded, exactly as for
`separate`. (An included envelope would co-member every pair in a
single-package repo, capping precision at |R|/C(n,2) even for a perfect
split.) Report `precision = |P∩R|/|P|`, `recall = |R∩P|/|R|`, `F1 = 2PR/(P+R)`.
Reference trees deliberately under-specify: unlisted files and unlisted
structure carry no opinion. F1 is reported per mode alongside assertion
verdicts; it never gates by itself in batch 1.

### Candidate distinctness observation (QUAL-P2-1)

Diversity machinery currently cannot be assumed to produce distinct candidates.
For every evaluated result the harness additionally records, without gating:
number of candidates returned, whether `solutionSpaceConverged` is true, and
whether candidates 1..k have pairwise-distinct trees (compare membership sets).
This observation feeds the diversity investigation; targets must not depend on
candidates beyond index 1.

## Hand-validation protocol

Each batch includes at least one target hand-validated against the current
binary before commit: run the exact `[run]` invocation, evaluate every
assertion by hand from the JSON, record observed values and verdicts in the
stream notes, and change neither fixture nor target to make a failing assertion
pass. A failing assertion on hand-validation is a finding about strata, which
is the corpus working as intended.
