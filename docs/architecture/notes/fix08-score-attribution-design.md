# FIX08 design study — per-move score attribution

Status: DESIGN STUDY ONLY — nothing here is implemented in FIX08. The
`SymbolMove.delta` shipped today carries the pass's acceptance delta (the
strict-J margin the relocation earned at acceptance time); this note records
what a fuller attribution would cost and recommends when to build it.

## The question

A file move narrates one number (its ΔJ) because the moving unit is one SCC.
A symbol move is different in kind: it rides on top of an already-polished
file layout, its acceptance was priced against a running best that earlier
relocations moved, and its effect is spread across six J(T) terms. When a
user asks "why did `charge` move to `checkout.py`, and what did it buy?", the
honest answer needs more than the scalar delta.

## Constraint that shaped FIX08

The cut term normalizes by the candidate's own edge-weight mass
(`Σ w(e)·h(lca) / (MAX_CROSSING_HEIGHT · Σ w(e))`, D-37). Two consequences
bound any attribution work:

1. A single symbol move can improve cut by at most ≈ `1/16` (consolidating
   every crossing edge to file height), while the anchored mode prices each
   moved node at `μ/n`. On small or sparsely-connected graphs the anchor term
   therefore dominates every achievable cut gain, and an anchored symbol pass
   rightly holds still. Attributions must expect many "no move" outcomes to be
   *correct*, not failures.
2. Because cut is mass-normalized, a move's cut share depends on edges that
   have nothing to do with it. Per-edge contribution lines are only meaningful
   as before/after differences on the SAME candidate, never as absolute
   shares.

## Options considered

### Option A — extend the acceptance-delta log with per-term deltas

At each accepted relocation, record the six-term breakdown of
`score_with(&overlay)` before and after (the scorer already returns
`ScoreBreakdown`; two calls exist at every trial). Store
`terms_before/after` alongside `delta`.

- Cost: small. The breakdown is computed anyway inside `score()`; keeping the
  pair adds one struct to `SymbolRelocation` and ~12 floats per accepted move.
  No new solver passes, no re-scoring.
- Fidelity: exact for the terms, but "before" is the running best, so term
  deltas are path-dependent (greedy order shows up in the numbers).
- Schema impact: additive DTO fields; RESULT_SCHEMA_VERSION unchanged.

### Option B — leave-one-out shadow rescoring

For each accepted relocation, recompute J with the relocation undone but all
other acceptances kept, and report `J(without_i) − J(final)`.

- Cost: one full re-score per accepted relocation (assembling + scoring the
  candidate tree). Acceptable offline; noticeable in `analyze` hot paths.
- Fidelity: answers the user's real question ("what breaks if I keep
  everything else but reject this move?"), which Option A's path-dependent
  deltas only approximate.
- Schema impact: same as A plus a definition users must learn.

### Option C — scorer instrumentation (per-edge, per-term contributions)

Thread per-edge crossing heights and per-container imbalance shares through
`score_candidate` into the result, letting narration cite specific edges.

- Cost: large — touches core's scorer surface consumed by ranking, pack, and
  diversify; risks coupling the DTO to scorer internals (D-37's boundary).
- Fidelity: highest, but the numbers are exactly the ones normalization makes
  treacherous (consequence 2 above). High risk of over-explaining noise.

### Rejected outright

Shapley-value / permutation attributions over relocation orderings: exact and
order-independent, but factorial-cost or sampling-approximate — unjustifiable
for a narration aid.

## Recommendation

Option A now (cheap, exact per-term, additive schema), Option B later behind a
diagnostic flag if users need counterfactual "what if I decline this move"
answers. Do not build C without first revisiting D-37: per-edge exposure is a
scorer-internals leak dressed as a feature.

## Related finding recorded during FIX08

Under greenfield coefficients (μ = 0) the symbol pass can satisfy the same
cut pressure by migrating CONSUMERS toward the misfiled symbol instead of
moving the symbol itself — cut-equivalent paths with no anchor price to break
the tie. This is not a defect of the pass (it accepts only strict-J
improvements), but it means greenfield symbol narrations may read backwards
("moved callers to `charge`" rather than "`charge` moved to its callers").
Anchored mode does not exhibit this. If it matters product-wise, the lever is
a tie-break preference for moving the lower-fan-in node, not a coefficient
change — out of FIX08 scope by charter.
