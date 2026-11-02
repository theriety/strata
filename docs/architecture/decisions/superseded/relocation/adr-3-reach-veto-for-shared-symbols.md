> **Status:** Superseded
>
> **Superseded by:** [ADR-6 — Symbol destinations obey pass-start guardrails](../../relocation/adr-6-symbol-destination-guardrails.md)
>
> **What changed:** Partial change: destination admission now also preserves pass-start type-only file roles and outbound destination-folder dependency envelopes.
>
> superseded-by: adr-6

# ADR-3: A shared symbol is protected by a reach veto, not by a score term

- **Status:** Accepted
- **Date:** 2026-08-27
- Accepted by owner ruling after four measured candidate designs; see Context.

## Context

Analyzing `~/Repositories/ai` emitted:

```
 - move `ComputerUseConfig` from src/adapters/types/tools.ts
   to src/adapters/openai/dispatch/prepare.ts
```

`ComputerUseConfig` is consumed from `adapters/anthropic/tools.ts` **and**
`adapters/openai/tools.ts`. Burying it inside one adapter makes the other
depend on that adapter's internals.

The objective cannot see this. `cut_cost` prices how *high* a dependency
crosses and nothing else, so the edge `anthropic/tools.ts → adapters/types/`
and the edge `anthropic/tools.ts → adapters/openai/dispatch/` have the same
LCA (`src/adapters`) and cost exactly the same. Both candidate homes are Files
at the same depth, so no priced term separates them — measured pre-fix, every
term tied to the last digit. What separates them is **direction**, and no term
prices direction.

## Candidates measured, and why each was rejected

**Penetration depth as a J term.** Price how far below the LCA the
depended-upon end sits. Rejected: both homes are Files, so a level-based
measure is identically zero, and the hop-count variant could not tell burying
a shared symbol inside one user apart from moving a user next to what it
depends on — the two produce the identical tree. Taxing the second blocks the
move sequence a clusterer assembles a domain out of, and measurably did: the
greenfield search returned `J = 0.3548` where a `0.125` layout existed.

**Layered centrality and clustering.** Measure the symbol's centrality between
its own file's cluster and each consumer cluster. Rejected on three
independent measurements: label-propagation put `ComputerUseConfig` in the
**anthropic** community (50 members), voting for the bad home; implemented as
a J term it left the suggestion in place (delta only `-0.0014` → `-0.00064`)
and re-broke `should_name_a_balanced_cluster_by_the_shared_home_prefix`; and
the dilution arithmetic needed `epsilon = 1.6`, adding `+0.86` to every J.
The participation coefficient itself separates the cases cleanly (`0.50`
shared vs `0.00` private) but is **placement-invariant**, so it cannot rank
two candidate homes.

**Barrel-footprint encapsulation as a J term.** Require a symbol a barrel
re-exports to stay inside the common ancestor of that barrel's other exports.
Correct in a unit witness, but inert on the real repository: raising `epsilon`
from `0.3` to `1.0` left the greenfield delta bit-identical
(`-2.5603266493590127e-05`), proving its contribution to that delta was
exactly zero, because the assembled tree collapses all 21 barrel members into
one container. A term-scale defect compounds this: on the current tree
`capacity` reads `86.4` and `imbalance` `10.119` while every other term sits
near `0.2`, so any new term is rounding error until that is addressed.

**Other measures swept.** Weighted 1-median picks the folded home
(`4.20` vs `5.40`); weighted 1-center ties (`4` vs `4`); edge betweenness is
`0` because a type is a graph sink; git co-change has no data (46 commits, the
file touched once inside a 69-file foundational commit).

## Decision

Refuse the move rather than price it. `ReachGuard`
(`crates/engine/src/analyze.rs`) bars any `SymbolPass` relocation that would
force something depending on the symbol to depend on a folder it does not
already depend on. The check is static and upstream of scoring, exactly as the
source/test boundary veto is (ADR context: FIX11 D-3), never a penalty the
other six terms can outvote.

Three rules define it:

- **Folder grain.** A dependency between modules is written at folder grain, so
  a move inside one folder rearranges nothing a dependant can see.
- **Hoists are exempt.** A move into a folder that already contains the origin
  is the direction the guard exists to protect. Because the internal tree keeps
  one flat slash-keyed folder per real directory and only the render boundary
  nests them, that ancestry is read off the folder key, not the parent chain.
- **Only inbound reach counts.** The destination gaining dependencies of its
  own is the symbol's coupling travelling with it, which the objective already
  prices. What it cannot price is a third party handed a neighbour it never
  asked for.

## Consequences

Measured on `~/Repositories/ai`, both modes:

| | before | after |
| --- | --- | --- |
| suggested symbol moves | 609 | 547 |
| `ComputerUseConfig` suggestion | present | **absent** |
| production→production burials | 67 | **0** |
| moves inventing any inbound module edge | 129 | 52 |

Every residual inbound edge has a `spec/` module as the reaching party — edges
the engine zero-prices by the FIX11 test-zone tie-cut, by design.

Gates: workspace suite 684/685, sole red the standing D-50
`cycle_span_witnesses_import_cycle_tearing`; parity goldens pass **un-blessed**,
so no re-bless was required; clippy `-D warnings` and `cargo fmt --check` clean.

**Known limitation.** The guard misses a fold whose destination module already
depended on the source. Four symbols on `~/Repositories/ai` are affected —
`FrozenPrefix`, `CapabilityOverride`, `Profile`, `BatchOutputItem` — seven
suggestions across the two modes. They remain suggested and remain wrong.
Closing them needs a signal that survives an already-present edge; none of the
four candidates above supplied one.

The encapsulation term was not kept. Its measured contribution to this defect
was zero, and the guard subsumes its intent without a coefficient to calibrate.
