# ADR-12: Conservative companion-owner affinity

📌

The IR carries conservative, adapter-detected `CompanionOwner` affinities separately from dependency edges, and each profile charges a directional `companion-separation` cost while a signature companion type is kept away from its owner's file.

- Status: `Accepted`
- Date: `2026-08-31`

## 🎯 Motivation

A signature type can belong beside the function or method whose contract it defines even when ordinary dependency counts favor its current file or another consumer. A single consumer is insufficient ownership evidence: shared and intentionally universal types often begin with one consumer, and treating that accident as ownership would create false-positive relocations.

## 🧭 Context

The relationship is semantic placement evidence rather than a dependency. Feeding it into the dependency graph would incorrectly alter cuts, cycles, reach, capacity, polarity, and relocation guards.

## ✅ Decision

The language-neutral IR carries affinities separately from dependency edges. `CompanionOwner` relates an owner declaration to a companion type. Adapters preserve affinities through fragment merging and node-ID remapping.

The TypeScript adapter considers only types referenced in function and class-method signatures. A candidate type must end in `Params`, `Options`, `Input`, `Output`, `Result`, `Context`, or `State`. After removing that suffix, camel-case and separator tokens are normalized, grammatical `to` and structural `Adapter` tokens are ignored, and `-ing` forms are reduced. At least two semantic tokens must exactly equal the function name or enclosing-class-plus-method name. The adapter emits an affinity only when exactly one owner matches. Body-only references, runtime declarations, generic one-token names, and ambiguous matches provide no affinity.

Each parameter profile has a directional `companion-separation` objective coefficient, defaulting to `0.05`. A companion is separated unless it occupies its owner's immutable analysis-start file. Moving the companion to that file removes the charge; moving the owner into the companion's file does not. A value of `0.0` disables the term for that profile. Negative and non-finite values are invalid.

Analysis results use schema version 6. Score breakdowns expose `companionSeparation` in JSON and `companion-separation` in configuration and human output.

## 🔀 Alternatives considered

**Use a single signature consumer as ownership evidence.** Rejected because current consumer count does not distinguish universal contracts from owner-specific companions.

**Infer ownership from filenames or documentation.** Rejected because those hints are language- and repository-convention dependent and can drift independently of the signature contract.

**Represent affinity as a dependency edge.** Rejected because placement preference must not manufacture dependency topology or affect structural findings and guards.

**Reward either declaration moving toward the other.** Rejected because ownership is directional: the companion belongs with the immutable owner, not vice versa.

## ⚖️ Consequences

Strongly named signature companions can overcome weak competing placement pressure without relaxing existing relocation safeguards. Conservative matching intentionally misses uncertain ownership rather than guessing. Profile scores, gains, candidate ordering, and narrated deltas include the same directional term, and saved-result consumers must migrate to schema version 6.
