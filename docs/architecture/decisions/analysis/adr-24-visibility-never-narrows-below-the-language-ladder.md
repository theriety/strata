# ADR-24: Visibility findings never narrow below the language's spellable scopes

📌

The Rust adapter states which scopes Rust can spell for each restricted declaration, and the visibility finding raises its derived scope to the smallest such scope, so strata no longer advises a narrowing Rust cannot express.

- Status: `Accepted`
- Date: `2026-10-01`

## 🎯 Motivation

Self-analysis reported `pub(super)` items in `crates/cli/src/args.rs` and `dispatch.rs` as "exported at Package but needed only at Folder". Both files are children of the binary root `main.rs`, which consumes the items. In Rust, `pub(super)` from a crate-root child is crate-wide, and no narrower spelling reaches the root file. Eleven findings advised something the language cannot say.

## 🧭 Context

The finding compares a declaration's declared scope with the lowest common ancestor of its consumers. That derived scope can be any container, but Rust offers only a ladder: private (own module subtree), `pub(super)` (parent subtree), `pub(in path)` (an ancestor's subtree) and `pub(crate)`. A derived scope that falls between two rungs cannot be written. The user decided the rule lives at the Rust provider level so that no language's restrictions fold into core logic.

## ✅ Decision

**The adapter states the ladder.** For each restricted declaration, the Rust adapter emits `VisibilityScope.expressible`: one `ScopeRung` (file set plus optional definition file) per enclosing module from the declaring module up to the crate root. If any rung cannot be resolved, the ladder is dropped whole, so a missing rung can never suppress a finding.

**The engine states a neutral floor.** While projecting sidecars, the engine converts each rung to a `ScopeLevel` and records `IntermediateRepresentation.scope_ladders` (node id plus ascending levels), serialized only when non-empty. `visibility_violations` raises each finding's derived level to the smallest ladder level that covers it, and drops the finding when the declared level is no wider. The engine and core hold no Rust concepts; a node without a ladder (TypeScript, Python, plain `pub`) behaves as before.

**Scope is the finding only.** `derive_visibility`, the symbol relocation planner and its reach guards (ADR-21, ADR-22) are untouched, so moves and plans are unchanged.

## 🔀 Alternatives considered

**Clamp `Node.visibility` at merge time.** Rejected: it falsifies the declared level in messages and tree output, and goes stale when the planner moves files.

**Special-case crate-root children in the engine.** Rejected: it puts a Rust rule in the language-neutral layer and misses `pub(in path)` ladders.

**Apply the floor inside `derive_visibility`.** Rejected: the planner shares it, and its guards must never be weakened.

## ⚖️ Consequences

The crate-root `pub(super)` false positives disappear; a `pub(super)` item used only in its own subtree, and a plain `pub` item used only under its folder, are still flagged. The sidecar is additive and defaults to empty, so the IR schema stays at version 3 and the JSON report schema stays at 9. Snapshots of Rust code with restricted items gain the field and therefore a new content hash. Module scopes are cached per analysis pass, so crate-root subtrees are walked once.

**Also covers: rungs that share a level with the need but miss a consumer.** In a `foo.rs` + `foo/` module the private rung (the item's own subtree) and the parent's folder can project to the same `ScopeLevel`, so a level-only floor treated `pub(super)` as narrowable when the consumer was the parent definition file (`render.rs` using `render/report.rs`), a sibling reached through the parent (`parse/references.rs` used by `parse/declarations.rs`), or the crate root (`lib.rs` using `bind.rs`). Rungs are therefore filtered while the ladder is projected: a rung stays only if its file set contains the file of every placed consumer (one the ladder can find). A declaration whose consumers all sit inside a rung keeps that rung, so a `pub(super)` item used only inside its own module is still flagged. The filter is file-set based and Rust-free: the adapter still states the rungs, and the engine only checks them against the graph's edges.

Consumers are read after re-export flattening, the way `derive_visibility` reads them: an edge that targets a re-export node counts as a use of the original declaration, so a `pub(super) use` facade never hides a real consumer outside a rung. A consumer that lies outside every rung (a `cfg(test)` module the analyzer never defined, a file outside the module tree) is unplaced and is left out of the rung check. `derive_visibility` still counts it in the derived need, so the floored level is never below that need. An item whose only unplaced consumer sits in a folder whose rung shares the need's level label may still be flagged, the same result as having no ladder.
