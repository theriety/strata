# ADR-18: Move endpoints are real paths under see-through source roots

📌

Every move endpoint Strata prints is a real repository path, because namespace derivation now aligns physical directories with logical keys while skipping see-through source roots.

- Status: `Accepted`
- Date: `2026-09-25`

## 🎯 Motivation

For `crates/core/src/diversify.rs`, the physical directory `crates/core/src` shares no suffix with the logical key `crates/core`, so the whole physical directory became the namespace and the key was appended again:

```text
from: crates/core/src/crates/core            (does not exist)
to:   crates/core/src/crates/engine/analyze  (does not exist)
```

Every file sitting directly in a see-through root is affected, not only cross-package moves.

## 🧭 Context

[ADR-8](../relocation/adr-8-physical-layout-governs-relocation-and-capacity.md) requires move endpoints to be physical, dataset-qualified paths with transparent segments visible. Clustering keeps a separate logical folder key in which source roots such as `src`, `spec`, `test`, `tests`, `lib`, `dist`, and `__tests__` are see-through.

When Strata places a file into a new group, it keeps the file's real folder prefix, its namespace, and adds the group folder beneath it. That grouping is intended. The namespace, however, was computed as the part of the physical directory not covered by a literal common suffix with the logical key. A see-through root breaks that suffix match.

## ✅ Decision

Every endpoint Strata prints is a real repository path.

- `from` is always the file's actual pass-start physical parent directory.
- A `to` naming an existing folder is that folder's physical path.
- A `to` naming a new group folder is the physical directory that keeps the members' namespace, followed by the group name. For files in `crates/core/src`, a new group is `crates/core/src/<group>`.

Namespace derivation understands see-through roots. It aligns the physical directory with the logical key while skipping see-through segments, rather than requiring a literal suffix. One derivation serves narration and relocation admission, so the namespace a move is checked against is the namespace it is reported in.

Every printed `from` exists at pass start. Every printed `to` either exists or is a folder the same candidate creates, whose parent exists or is itself created by that candidate. Tests assert this invariant across the golden fixtures.

## 🔀 Alternatives considered

**Keep the current output as a grouping label.** Rejected because `from` named a folder that does not exist, so the advice could not be followed with `mv` and contradicted ADR-8.

**Real `from`, label-only `to`.** Rejected because [ADR-16](adr-16-plain-text-reports-and-relocation-endpoints.md) requires endpoints a reader can follow between the before and after trees.

**Stop treating source roots as see-through.** Rejected because transparency is what lets clustering relate `src` and `tests` code; only the path projection was wrong.

## ⚖️ Consequences

Reports and JSON endpoints can be applied as printed. Golden fixtures with files directly in a see-through root are re-blessed. Because admission shares the corrected namespace, some namespace-boundary decisions change, which can alter candidates; those shifts are expected and reviewed with the golden updates.
