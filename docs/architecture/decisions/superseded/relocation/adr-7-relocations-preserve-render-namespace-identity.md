> **Status:** Superseded
>
> **Superseded by:** [ADR-8 — Physical layout governs relocation and capacity](../../relocation/adr-8-physical-layout-governs-relocation-and-capacity.md)
>
> **What changed:** Partial supersession: physical move presentation now preserves complete dataset-qualified paths, Folder capacity uses immediate physical entries, and symbol relocation cannot descend into a deeper physical directory; the opaque pass-start namespace boundary and non-relocatable `FileBody` rules remain effective under ADR-8.
>
> superseded-by: adr-8

# ADR-7: Relocations preserve render namespace identity

- **Status:** Accepted
- **Date:** 2026-08-29

## Context

Strata renders file placements from a laminar scope that deliberately omits
transparent path prefixes. Two files can therefore have the same normalized
folder while belonging to different pass-start render namespaces. A relocation
that treats only the normalized folder as identity can move a file or symbol
across that hidden boundary, even though the resulting path changes its
structural namespace.

Normalized-folder identity is also too coarse for rendered leaf safety. Exact
equal basenames in one rendered destination collide only when they belong to
the same namespace; equal basenames in different namespaces are distinct valid
paths. An initial guard that ignored namespace rejected valid source pairs and
left both analysis modes with zero candidates.

Executable top-level statements present a related identity problem. They were
previously represented as ordinary symbols, which made file-scope behavior look
independently relocatable. That behavior belongs to its containing file even
though its SLOC and dependency edges remain relevant to analysis.

## Decision

File and symbol relocation preserve an opaque pass-start render namespace as a
hard structural boundary. Namespace identity is derived privately from the raw
file directory after removing its exact normalized laminar scope. The same
derivation covers package roots and synthetic workspace folders; it does not
recognize particular directory names or application concepts.

Destination permissions are frozen before relocation begins. Existing clusters
retain the namespaces represented by their pass-start members, while newly
created clusters inherit the namespace set of their founding members. Later
arrivals cannot expand either set. A file may join only a destination permitted
for its namespace, and a symbol may move only between files with equal
pass-start namespace identities. Final candidates are checked against the same
invariant.

Rendered file identity is the pair of namespace and exact basename at a
rendered destination. The basename comparison is case-sensitive and includes
the extension. A destination may not contain two distinct files with the same
identity. Equal basenames remain valid across namespaces or rendered
destinations.

Executable file-scope content is represented as `NodeKind::FileBody`.
`FileBody` nodes retain attributed SLOC, dependency edges, and their runtime
role in mixed-file classification. They cannot relocate independently at
symbol grain, but remain mobile with their whole file when that file stays
inside its pass-start namespace.

These rules are admission constraints rather than scoring terms. Existing
pass-start dependency and file-role guardrails continue to apply independently.

## Alternatives considered

**Normalized folder as the complete identity.** Rejected because transparent
namespaces can normalize to the same folder. Applying basename uniqueness at
that level also rejects distinct valid paths and can eliminate the entire
candidate set.

**Global basename uniqueness.** Rejected because equal basenames are valid in
different namespaces and rendered destinations. The collision boundary must
match the path identity that rendering preserves.

**Mutable destination permissions.** Rejected because an accepted arrival
could authorize a later cross-namespace move, making validity depend on move
order rather than pass-start structure.

**Recognize executable file bodies by a sentinel name.** Rejected because a
name-based exception confuses syntax with semantic role and cannot provide a
language-neutral IR contract.

**Apply the boundary as a score penalty.** Rejected because a structural
identity invariant must not be traded against objective improvement.

## Consequences

Neutral generated tests first demonstrated both file-grain and symbol-grain
namespace crossings, then passed after the structural guard was applied.
Positive controls retain same-namespace relocation, namespace-separated equal
basenames, case-distinct basenames, and whole-file mobility. The engine suite
passes all 200 tests.

On the pinned read-only repository analysis, anchored and greenfield modes each
retain one candidate. The four confirmed cross-namespace file transitions and
the confirmed executable-file-body transition are absent. The workspace test
run retains only the previously documented D-50
`cycle_span_witnesses_import_cycle_tearing` failure; no additional workspace
failure was introduced.

The implementation adds no public configuration, result, evaluation-corpus, or
scoring field. Namespace identity remains private engine metadata. Adding
`NodeKind::FileBody` is an additive serialized-IR change: new binaries can read
older snapshots, while older binaries may reject snapshots containing the new
variant.
