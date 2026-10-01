# ADR-19: A trait impl is one movable unit

📌

The Rust adapter emits each `impl Trait for Type` block as one declaration named from its header, so Strata can never propose moving part of a trait implementation.

- Status: `Accepted`
- Date: `2026-09-25`

## 🎯 Motivation

Strata could suggest moving one method of a trait implementation, such as `fmt`, to another file. Rust requires a trait implementation to be one block, so that advice could never compile.

A first fix emitted the whole block as one declaration named after the self type. A type with several trait impls then produced several declarations with the same name as the type itself, which made reports ambiguous.

## 🧭 Context

The Rust adapter emitted each method of an `impl Trait for Type` block as its own declaration. Rust allows a trait impl to live in any module of the crate that owns the trait or the type. Inherent impls (`impl Type { ... }`) may appear as several blocks per type.

## ✅ Decision

The Rust adapter emits each `impl Trait for Type` block as exactly one declaration. Its methods, associated types, and constants are not separate declarations; their source lines and outgoing references belong to the block.

The declaration is named from the block's header, without the impl's own generic parameter list: `impl Display for Report`, `impl From<Config> for Profile`. The header is taken as written, so the name is unique per file but not within a crate: `impl fmt::Display for Report` and `impl std::fmt::Display for Report` name the same kind of impl differently, and two modules may each write an identical header for different types in scope. The symbol pass's name-collision check refuses to move a block into a file that already holds a declaration of the same name. Mutually exclusive `cfg` variants of the same header are disambiguated by source order.

The block may move on its own, subject to the ordinary guards. Rust allows a trait impl to live in any module of the crate that owns the trait or the type, and [ADR-17](../relocation/adr-17-relocations-stay-inside-their-package.md) keeps moves inside one crate by default.

Inherent impls (`impl Type { ... }`) are unchanged. Rust allows several inherent blocks per type, so their methods remain individual declarations.

The block uses an existing declaration kind. No IR schema change is needed.

## 🔀 Alternatives considered

**Name the block after the self type.** Rejected because several impls of one type produce indistinguishable declarations.

**Keep methods separate and glue them with an IR marker.** Rejected for now because it needs an IR schema bump and a new symbol-pass rule for detail that reports rarely need. It remains the path if per-method detail becomes necessary.

**Fold the impl into its type.** Rejected because impls often sit apart from the type on purpose, such as `impl From<A> for B` next to `A`.

## ⚖️ Consequences

Strata can no longer propose moving part of a trait impl. Trees and JSON show one entry per trait impl instead of its methods. Symbol counts and SLOC in Rust fixtures change and are re-blessed. Crossing a crate with a trait impl is only possible with the ADR-17 switch on, where the orphan rule is not modeled.

The impl block keeps the type declaration kind. A file that holds only trait impls therefore counts as type-only, and the block is priced at type affinity. It needs a strictly greater pull toward the destination than a plain type to move. Fixtures shift accordingly: two scores in the `workspace-rust` goldens move, 0.6929 to 0.6958 and 0.8929 to 0.8958.
