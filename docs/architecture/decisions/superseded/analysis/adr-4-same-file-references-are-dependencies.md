> **Status:** Superseded
>
> **Superseded by:** [ADR-9 — Complete analysis parameter profiles](../../superseded/advice/adr-9-complete-analysis-parameter-profiles.md)
>
> **What changed:** Partial: same-file references remain dependency edges, but their cut price now uses independently configurable runtime and type affinity in each complete parameter profile.
>
> superseded-by: adr-9

# ADR-4: A same-file reference is a dependency edge

- **Status:** Accepted
- **Date:** 2026-08-27
- Accepted by owner ruling on three decisions taken before implementation; see
  Decision.

## Context

Analyzing `~/Repositories/ai` emitted:

```
 - move `GOOGLE_DEFAULT_CONTENT_TYPE`
   from src/generators/synthographers/adapters/google/codec.ts
   to   src/generators/synthographers/batch.ts  (delta -0.0008)
```

The constant is declared at `codec.ts:57` and read at `codec.ts:110`, inside
`mapGeminiImageResponse` in that same file. That read was not in the graph. Its
only recorded dependants were three `ValueImport` edges out of other files, so
the origin appeared to have no claim on the symbol and the move looked free. It
was not: `batch.ts` already imports `findClosestGoogleAspectRatio` from
`codec.ts`, so carrying the constant across would have closed a circular import
— which the cycle veto could not see either, because the edge that closes the
cycle had never been recorded.

The root cause sat in the binders. `emit_references` resolved a referenced
identifier only through the module's `imported` table, while its siblings
`emit_calls` and `emit_inheritance` — in the same file, over the same
declaration — also consulted `module_local`. The Python binder stated the
reasoning outright:

> Only an imported reference is a cross-module value dependency; a bare
> same-module name reference carries no import to model here.

That is the error, and it is a category error rather than an oversight. These
edges are not an import list. The engine relocates symbols against them as a
**dependency graph**, and in that graph a same-file reference is the single most
important fact about whether a symbol may leave its file. A same-file *call*
already produced an edge; only a same-file *value* or *type* reference did not.

Measured on `~/Repositories/ai` before the fix, over 547 post-FIX13
suggestions, counting only references that survive comment stripping:

| | anchored | greenfield |
| --- | --- | --- |
| moves whose symbol is still used in its own file | 121 | 132 |
| …engine blind to that use | 54 | 65 |
| …and the destination already imports from the source | 21 | 27 |

Three blind shapes, one each: a constant read
(`GOOGLE_DEFAULT_CONTENT_TYPE`), a function passed as a value
(`toNative: deriveAnthropicNative`), and a same-file type reference
(`OpenAIRequest`).

A second, independent blind spot sat in the engine. `cyclic_vertex_count` sums
the members of every strongly connected component of size greater than one, so a
brand-new cycle closed between two files *already inside* a component leaves the
sum unchanged and the veto passes. Restoring the edges does not close that hole.

## Decision

**A referenced name resolves through the same-module declarations when the
imports miss, and the resulting edge is priced like any other.** The edge kind
follows the target: a type target is a soft `TypeReference`, anything else a
hard `ValueImport`. A declaration naming itself emits nothing.

Three sub-decisions were taken by owner ruling before implementation:

1. **Scope** — the binder fix ships together with cycle-detector hardening, not
   alone. Restoring the edges fixes what the veto can see; it does not fix what
   the veto can measure.
2. **Pricing** — same-file edges join the cut mean at the height the formula
   already assigns them, `height_penalty(ScopeLevel::File) = 1.0`. That is the
   floor, so `cut` falls repo-wide and the e2e goldens take a deliberate
   re-bless. The alternative — excluding them from the cut mean to protect the
   baseline — was refused as special-casing a term to preserve a number.
3. **False edges** — the edges are emitted hard and the false-edge rate is
   measured, not pre-empted. `ReferenceCollector` pushes every identifier
   unscoped, so property keys and shadowed locals are a real risk; guarding
   against a rate nobody had counted would have been a guess.

`cyclic_edge_count` joins `cyclic_vertex_count` as a second veto measure in
`SymbolPass`: the number of edges whose endpoints share a cyclic component. A
relocation is refused when **either** measure rises.

## Consequences

Measured on `~/Repositories/ai`, both modes, before and after:

| | anchored | greenfield |
| --- | --- | --- |
| suggested symbol moves | 263 → 227 | 284 → 247 |
| blind to a same-file use | 54 → 15 | 65 → 19 |
| …creating an import cycle | 21 → 6 | 27 → 8 |
| `cut` | 0.2097 → 0.1788 | 0.2098 → 0.1790 |

The graph gained 1,083 same-file edges (722 `ValueImport`, 361
`TypeReference`) on 4,248, a 25% rise. A 40-edge hand-classified sample found
**0 false edges**; four were verified against the source line by line. The cut
dilution is real and was accepted: `cut` fell about 15%, while `imbalance`
(7.48 → 7.76) and `capacity` (88.6, unchanged) still dominate the objective, so
no term's relative standing moved.

Of the 56 anchored suggestions the change removed, 37 are directly explained by
a restored same-file use. The remaining 19 are second-order re-pricing, and a
sampled read of them — `fetchAllBatches` into `adapters/google/errors.ts`,
`preflightAgentDispatch` into `dispatch/types/adapter.ts` — found cross-domain
scatters worth losing, not good suggestions destroyed.

The Rust binder needed no change, and this was measured rather than assumed: it
resolves references through the semantic database, which has no same-file
exclusion. Over `crates/core`, 723 of 1,169 edges are already same-file,
including 237 `TypeReference`.

`cyclic_edge_count` blocked **nothing** on `~/Repositories/ai` — with the branch
disabled the move counts are identical, 227 and 247. It survives on the strength
of its synthetic witness alone, as defence in depth against a shape this
repository does not happen to contain. That is an honest cost: a measure that
runs on every trial relocation and has so far never fired.

Two limitations remain, both filed as backlog rather than fixed here:

- **TypeScript type members.** The parser does not record the types a
  declaration's *members* reference, so `interface A { search?: B }` yields no
  `A → B` edge. This is what the residual 15 / 19 blind moves are, including the
  surviving `OpenAIRequest` and `GOOGLE_DEFAULT_CONTENT_TYPE` suggestions.
- **Sequential draining.** `SymbolPass` moves one symbol at a time and re-bases
  its cycle counts after each acceptance, so a file can be emptied one symbol at
  a time along a path where no single step closes a cycle. Both surviving named
  suggestions ride exactly this: the same-file user leaves first, and the
  constant then follows into a file that no longer depends on its origin.
- **The file-level refinement loop** at `analyze.rs` still calls
  `cyclic_vertex_count` alone, over the partition quotient. Widening it was
  deliberately excluded from this round.
