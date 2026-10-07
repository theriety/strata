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

7. **Names in any cfg the loader may drop count as uses, for advice only.** The loader runs with `cfg(test)` on and no feature, so rust-analyzer may drop any item or file behind another `#[cfg(..)]` (`not(test)`, a feature, a platform, `any(..)`) and nothing in it resolves. One parser predicate, `loader_may_drop`, decides: a cfg that is not certainly true with only `test` set (`all(test, feature = "x")` is dropped with the feature off); doubt counts as dropped. `cfg_test` polarity marking stays a separate predicate. Decision 10/3: "same rule for all cfg". Catch-all: any parsed file the loader never loaded (for instance one reached only through `#[cfg_attr(windows, path = "win.rs")] mod sys;`) is one maybe-dropped body: every identifier of it counts as a use from each of its declarations, its `pub` items are advice-exempt, and with no declaration its identifiers exempt the same-named ones. This subsumes unknown ways a file can fall out of the module tree. An inner `#![cfg(..)]` gates its whole file like a gated `mod x;`, and a `#[cfg_attr(..)]` on a `mod` declaration counts as maybe-dropped for that module's file and subtree. Every identifier in an item-position macro invocation (`cfg_if!`, at module, impl or trait level) counts as a use of the file, because the loader expands only the active branch; the adapter does not parse `cfg_if`. One broad rule, no per-position cases: a declaration whose syntax contains such an attribute anywhere inside (a statement, a field, a variant, a match arm, a trait item) is dropped as a whole, so all its identifiers are advisory. The adapter therefore treats every identifier in such code (such a declaration, an impl method, a `use` item of any visibility, or a file reached through `#[cfg(..)] mod x;` and its descendants) as a use of every same-named production declaration, and states it as `IrFragment.advisory_uses`. The engine counts the user's file as a consumer in the ladder verdict and nowhere else: advisory uses are never graph edges, so scores, cohesion, cycles and moves cannot change. This is conservative: a name collision may hide a finding but never creates a wrong one.

8. **Public API is never narrowed (R3).** An item reachable from a library crate's public paths is public API, and no finding narrows it. The Rust adapter asks rust-analyzer for each library target's module scope as seen from another crate, which already resolves named, renamed and glob `pub use` and drops every `pub(..)` import. A `pub` inherent method of a public type is public API too. A bin-only crate has none, `pub(crate)` and `pub(in ..)` never count, and `#[doc(hidden)]` still does. When unsure, the item counts as public API: no other crate to ask, a non-cargo project, a `pub` declaration the loader may drop (rule 7's predicate, ancestors included) or in a file it never loaded, unless its file belongs only to binary, test or example targets: a file a library also reaches (shared through `#[path]`) stays a library file. Library roots are matched on canonical paths so symlinked roots still count. The adapter states it as `Node.public_api`.

    Code the adapter cannot see is a different fact, stated as `Node.advice_exempt` ("may be used where the adapter cannot see"; it says nothing about outside users). Three sources: a `use` the loader may drop that is not private re-exports an item the analysis cannot see, so every same-named declaration is exempt; a dropped `use` in a file with no declaration has no node to act as advisory user, so its imported names exempt every same-named declaration; a dropped glob `use` (any visibility) imports names nobody can list, so every declaration, of any visibility, in the files of the module it names and its whole subtree is exempt. The glob path is resolved through the parser module tree; when it does not resolve exactly (an inline module, an external crate, an unknown path), or any file in the subtree holds a `use` that is not private (names flow onward), every declaration of the crate is exempt. Files are never matched by stem. Core `collect_findings` and the engine ladder read both flags through one predicate, `exempt_from_advice` (`re_export || public_api || advice_exempt`); anything that treats externally reachable nodes specially (the L06 test-only check) must skip both. Known gap: a private `use` plus `super::*` flowing through a glob target is not covered.
9. **Re-export floor.** A re-export (`pub use`, `pub(crate) use`, `pub(in ..) use`) exposes its target with reach (E0364, E0365): the adapter states an `Exposure` from the re-export node to its target, so the target is never advised narrower than the `use` declares, public API or not.

TypeScript and Python public API (package entry exports) is out of scope here.

**Inline modules.** An inline `mod m { .. }` shares its file with its parent, so file sets cannot tell the two scopes apart. The adapter drops inline modules from the ladder (their spelling keeps its place in the chain) and states no declared rung for an item that sits inside one, so strata stays silent.

**Equal labels.** A verdict whose declared and needed levels carry the same label is skipped, because "exported at Folder but needed only at Folder" would mislead.

## testOnly finding (report schema 10)

Where it lives: `crates/engine/src/analyze/findings/test_only.rs`; the visibility pass skips every node it reports, so an item gets one finding.

Rule: a node earns `testOnly` when the source does not declare it test code (`Node.test_source`, read before adapters demote production code that only tests use), it has at least one non-self-loop user, every user has `test_source`, no advisory use targets it, and it is neither `re_export` nor `advice_exempt` (trait-impl blocks are `advice_exempt`). Unknown users count as non-test. When unsure, silent.

Decision 10/3: **public API does not silence testOnly**, only visibility. Rule 8 ("never narrowed") is about narrowing a scope; "only tests in this repo use it" is a different claim and stays true for a public item. Wording claims only what strata sees: a non-public item says "`X` is used only by tests; move it under test-only code or delete it", a public one says "Only tests in this repo use `X`. It is public, so code outside this repo may use it too; check before moving or deleting it." TypeScript and Python adapters set `public_api` on every exported (TS) or non-underscore/re-exported (Py) item; exact entry-export detection (package.json `exports`/`main`, `__init__` re-exports) is deferred.

Module objects: a Python `import a`, `import a.b as c` and `from pkg import sub` (absolute or relative), a TS `import * as ns`, and a literal TS `require('./a')`, loaded by a non-test file, mark every declaration or export of the target module `advice_exempt`, so none is reported testOnly. This is advice-only in both languages: the dependency graph for these forms is unchanged.

Known limits (no code, may report a false testOnly for): non-literal `import()`, Python `getattr` / `importlib` with computed names, string registries, and names reachable only through `__all__` that start with an underscore.

## IR additions

`ScopeRung.spelling`, `VisibilityScope.declared_rung`, `IrFragment.exposures`, `IrFragment.advisory_uses`, `IrFragment.test_support_roots`, `Node.public_api`, `Node.owner` (the type a Rust impl member belongs to, used by testOnly's own-member rule), `AdvisoryUse.from_test` (the use comes from test code; visibility still counts it, testOnly ignores it), and a `ScopeLadder` verdict (`declared`, `needed`, `spelling`) replacing the level list. The adapter now emits an entry for every `Restricted`, `Crate` and `Public` declaration; `files` is set only for restricted ones, which keeps the declared-level override. All additions are serde-default and skipped when empty, so the IR schema stays at 3 and the report schema at 10.

## 🔀 Alternatives considered

**Keep the level floor.** Rejected: labels collide for distinct rungs and hide unreachable ones. **Flag exposure as an edge.** Rejected: it would change the dependency graph and about forty edge literals.

## ⚖️ Consequences

Fewer findings, none known false. The cost is over-suppression: a type exposed by a trait impl block is never flagged, and an unplaced or cross-crate user silences the item. Rust-only facts (rungs, spellings, exposure) stay in the adapter; the engine compares file sets.

## Amendment 2026-10-08: test-support crates

- **testOnly skips test-support crates.** The IR gains an additive, serde-default `test_support_roots` (repo-relative directories; IR schema stays 3). The Rust adapter states a workspace member as test support when it is `publish = false` (also inherited through `publish.workspace = true` from `[workspace.package]`), has a library and no binary target, no other member uses it through `[dependencies]` or `[build-dependencies]` (dev-dependencies do not count), and the workspace has at least two members. The engine skips items under those roots and knows nothing of Cargo. Severity is unchanged.
- **Limit.** Binary crates and a lone crate are excluded so an application never loses its findings. When every member would qualify (for example two independent unpublished libraries), none is flagged: nothing is left to be supported, so the rule fires only while at least one member stays a non-harness.
