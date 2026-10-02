# ADR-25: A visibility finding needs a strictly narrower scope the language can spell and that holds every user

📌

A visibility finding is emitted only when the language can write a scope strictly narrower than the declared one that still holds the home file and every user, signature exposure included. Otherwise strata stays silent. Rungs are compared by file set, not by level label.

- Status: `Accepted`
- Date: `2026-10-02`
- Supersedes: ADR-24 in part (the level floor and per-rung consumer filter are replaced by the rung comparison below)

## 🎯 Motivation

Suggestions are advice but must never be technically wrong. After ADR-24 the finding still advised scopes that do not compile or do not help: a trait named only in a `pub(crate)` fn bound (narrowing it trips `private_bounds`), a `pub(super)` item whose consumer is the `cfg(test)` parent module, a `pub(in path)` item already at its needed scope, and a `pub` fn whose only outside user is an integration test in another crate target.

## ✅ Decision

1. **Compare rungs by identity.** The needed rung is the narrowest rung of the adapter's ladder whose file set holds the home file and every consumer file. A finding is emitted only when that rung sits strictly before the declared rung. `ScopeLevel` labels are never compared to decide this; they only render the message.
2. **Consumers are all edges, all polarities, after re-export flattening.** If any consumer is in no rung (another crate, an unplaced file, `tests/eval.rs`), no rung holds it and nothing is emitted. Of several consecutive rungs with identical file sets the last wins.
3. **Exposure counts as use.** The adapter states `IrFragment.exposures`: a declaration whose signature names another type (fn parameters and return, generics, trait bounds, where clauses, pub field types, type alias right side, supertraits, impl headers). Every file of the exposer's declared rung counts as a consumer of the exposed type. A trait impl block states no scope, so a type it exposes is silent; a private struct field is body-only, an enum variant field is exposed. A header self type only counts as a use of the exposer's file.
4. **Unresolved means silent.** If the adapter stated a scope entry but its declared rung cannot be resolved, nothing is emitted.
5. **No ladder (TypeScript, Python):** emit only when the derived need is `File` and the declared level is above `File`.
6. **Message.** Keep "`X` in `path` is exported at A but needed only at B" and append "; narrowest spelling: `pub(super)`" (or "private (no `pub`)", "`pub(in crate::a::b)`", "`pub(crate)`") when the adapter gives one.

7. **`cfg(not(test))` names count as uses, for advice only.** The loader runs with `cfg(test)` on, so rust-analyzer drops `cfg(not(test))` items and files and nothing in them resolves. The adapter therefore treats every identifier in such code (an inline item, or a file reached through `#[cfg(not(test))] mod x;` and its descendants) as a use of every same-named production declaration, and states it as `IrFragment.advisory_uses`. The engine counts the user's file as a consumer in the ladder verdict and nowhere else: advisory uses are never graph edges, so scores, cohesion, cycles and moves cannot change. This is conservative: a name collision may hide a finding but never creates a wrong one.

**Inline modules.** An inline `mod m { .. }` shares its file with its parent, so file sets cannot tell the two scopes apart. The adapter drops inline modules from the ladder (their spelling keeps its place in the chain) and states no declared rung for an item that sits inside one, so strata stays silent.

**Equal labels.** A verdict whose declared and needed levels carry the same label is skipped, because "exported at Folder but needed only at Folder" would mislead.

**Library crates.** A `pub` item of a library crate is judged without outside-crate consumers; the public-API rule that protects it arrives in L05.

## IR additions

`ScopeRung.spelling`, `VisibilityScope.declared_rung`, `IrFragment.exposures`, `IrFragment.advisory_uses`, and a `ScopeLadder` verdict (`declared`, `needed`, `spelling`) replacing the level list. The adapter now emits an entry for every `Restricted`, `Crate` and `Public` declaration; `files` is set only for restricted ones, which keeps the declared-level override. All additions are serde-default and skipped when empty, so the IR schema stays at 3 and the report schema at 9.

## 🔀 Alternatives considered

**Keep the level floor.** Rejected: labels collide for distinct rungs and hide unreachable ones. **Flag exposure as an edge.** Rejected: it would change the dependency graph and about forty edge literals.

## ⚖️ Consequences

Fewer findings, none known false. The cost is over-suppression: a type exposed by a trait impl block is never flagged, and an unplaced or cross-crate user silences the item. Rust-only facts (rungs, spellings, exposure) stay in the adapter; the engine compares file sets.
