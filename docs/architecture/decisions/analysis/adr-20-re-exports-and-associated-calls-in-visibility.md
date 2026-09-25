# ADR-20: Re-exports and associated calls in visibility findings

📌

Re-export declarations are never subjects of the over-exposure finding, and Rust `Type::name` paths record a type reference, removing two classes of false-positive visibility warnings.

- Status: `Accepted`
- Date: `2026-09-25`

## 🎯 Motivation

Self-analysis showed two classes of false positive in the visibility finding.

First, a `pub use` re-export node has no direct callers, because callers resolve through it to the real item, so it was reported as too wide. Re-exports of a whole crate or module, such as `pub use strata_ir as ir;`, point at no single item and produce no re-export edge, so an edge-based exemption does not reach them.

Second, `PythonAdapter`, `RustAdapter`, and `TypeScriptAdapter` were reported as unused outside their crate even though other crates construct them with calls such as `PythonAdapter::new()`.

## 🧭 Context

The visibility finding reports declarations exported more widely than their callers need. A `pub use` line is a facade decision. A call such as `PythonAdapter::new()` names the type in its path, but the Rust adapter recorded no reference to the type. An earlier rule exempted only the source of a resolved re-export edge, which could not reach a re-export whose target is not a node.

## ✅ Decision

**Re-exports are never visibility-finding subjects.** Any declaration that is a re-export, whether of an item, a module, a whole crate, or a glob, is exempt from the over-exposure finding. The re-exported item is still judged at its own definition through its resolved callers. The rule lives in the language-neutral visibility pass. Adapters mark each re-export declaration with a node flag, `Node.re_export`: the Rust, TypeScript and Python adapters set it on every re-export node they emit (`pub use`, `export { x } from`, `export * as ns from`, and `__init__.py` import bindings). Core exempts every flagged node.

**Associated paths reference their type.** In `Type::name` expressions, such as `Type::new()` or `Type::CONST`, the Rust adapter records a type reference to `Type` when the qualifier binds to a type declaration. A module qualifier, as in `render::report()`, records none, and an unresolved qualifier falls back by name only onto a type. These references count as dependency edges everywhere, not only in the visibility finding.

## 🔀 Alternatives considered

**Model crate roots as nodes and emit a re-export edge to them.** Rejected because it adds a new node kind to every adapter's output to decide a case the facade rule already settles.

**Drop alias nodes in the Rust adapter.** Rejected because it works only for Rust, and it hides a real declaration from the graph.

**Accept the leftover warnings and document them.** Rejected because known false positives in every report teach users to ignore the finding.

**Count associated paths only for visibility.** Rejected because a constructor call is a real dependency; hiding it from scoring would keep the dependency graph incomplete.

## ⚖️ Consequences

The finding no longer flags facades or constructed types. A truly unused re-export is no longer reported as too wide. Associated-path references add edges, so scores shift slightly and fixtures are re-blessed.

The flag moves the IR schema from version 2 to 3. The change is additive: the flag defaults to `false` and is omitted when unset. New binaries still read version-2 snapshots and derive the flag from their re-export edges. Older binaries may reject version-3 snapshots.
