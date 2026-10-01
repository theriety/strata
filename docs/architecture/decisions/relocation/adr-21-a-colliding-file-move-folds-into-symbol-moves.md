# ADR-21: A colliding file move folds into symbol moves

📌 No printed file move may land on a path another file already holds. Such a move is withdrawn, and the file's declarations are offered to the file already there as symbol moves, which are accepted only if they pass every check an ordinary symbol move passes.

- Status: `Accepted`
- Date: `2026-09-27`

## 🎯 Motivation

A file move lands the file at `<destination folder>/<its file name>`. Nothing stopped another file from already holding that path. With the package wall lifted (`allow-cross-package-moves`, see [ADR-17](adr-17-relocations-stay-inside-their-package.md)), the `workspace-move-rust` fixture proposed:

```text
Move file crates/util/src/lib.rs → crates/core/src
```

`crates/core/src/lib.rs` already exists. Following the advice with `mv` would overwrite core's crate root. The solver meant "util's declarations belong with core", and a file move was the wrong way to say that.

## 🧭 Context

The file polish assigns each file (strictly, each strongly connected component of the file graph, an SCC) to a folder cluster. The pass-start layout is the one the polish starts from. Display levels above the folder level (domains and packages) are drawn afterwards.

A collision is not always the polish's doing. In the fixture above, util's `lib.rs` never left its pass-start folder cluster. The display levels drew the whole folder under core's package, because newcomers from core had joined it, and so carried the file along. Sending the file back to its home cluster changed nothing, and the same collision was found again every round.

After the polish, test files follow their subjects in a separate pass (the test-follower pass). A follower can change which package a folder is drawn in, so it can carry another file onto an occupied path after collisions were first checked.

The symbol pass then moves individual declarations between existing files. Every symbol move passes a fixed set of vetoes, including the two reach guards: a symbol may not give one of its dependants' folders a dependency on a folder it never depended on, and may not give the destination folder an outbound dependency it never had ([ADR-6](adr-6-symbol-destination-guardrails.md)). The inbound guard's definition has one carve-out: a neutral unoccupied sibling branch under the consumers' common ancestor ([ADR-11](adr-11-relocation-ownership-scoring.md)) may be given. Test-polarity declarations are pinned in place ([ADR-22](adr-22-test-symbols-are-pinned-by-polarity.md)).

A module root is the file that defines its module: a Rust crate or module root (`lib.rs`, `main.rs`, `mod.rs`), a TypeScript `index.ts`, `index.tsx`, `index.mts` or `index.cts`, and a Python `__init__.py`, `__init__.pyi`, `__main__.py` or `__main__.pyi`.

## ✅ Decision

**No printed file move lands on a path another candidate file holds.** When one would, the move is withdrawn. The file's declarations are offered to the file already at that path as one unit of symbol moves (a *fold*), unless the file is a module root. This holds in every profile and in both wall modes.

Withdrawal:

- The file that keeps the path is the one staying put; if every claimant moves, it is the claimant with the smallest path. Every other claimant is withdrawn.
- Only the colliding file is withdrawn. If the polish carried it out of its pass-start folder cluster, it returns there.
- If it is still in its pass-start cluster, the display levels are carrying it. The display levels then keep its folder in the file's pass-start package: that folder's domain and package clusters are split from unpinned ones, and its package is named after the pin rather than elected. When the file is offered as a fold, the cluster's newcomers (files that joined it from other pass-start clusters) are evicted toward the occupant's cluster, the folder they were being drawn into anyway.
- An eviction is a move the polish never priced, so each evicted newcomer must pass what a polish move passes: the relocation identity guard (package wall, namespaces, and no two same-named files in one folder), the folder capacity veto, and the quotient-cycle veto. A newcomer refused by any of them returns to its own pass-start cluster instead.
- A module root is never folded: retiring it would break the module it defines. Its colliding move is withdrawn and its folder pinned, and nothing else changes.

Settling: withdrawal runs on the polished layout, then the test-follower pass runs, and both repeat until a round withdraws nothing (bounded by the file count). So a follower cannot carry a file onto an occupied path. A faithful layout renders as the current tree and moves no file, so nothing is withdrawn from it, and a layout that settles faithful keeps no fold: a fold only ever replaces a file move the report shows.

Folding: the symbol pass offers each fold before its ordinary sweep. The unit is the file's declarations other than its executable file body and other than test-polarity declarations pinned by [ADR-22](adr-22-test-symbols-are-pinned-by-polarity.md); a pinned member stays in the source file and does not refuse the unit. All members move or none do. Each member must pass every veto an ordinary symbol move passes, the two reach guards included, exactly as ADR-6 defines them: the inbound guard with its neutral-branch carve-out and the outbound guard with none. No profile, configuration key, flag or code path skips, widens or narrows either guard. The unit must also fit the destination file's capacity, raise neither cycle count nor the visibility finding count, and improve the objective. A fold may drain the source file of every production declaration, because that file is being retired.

Refusal: if the symbol pass refuses a fold, the file stays where it is and the evictions its withdrawal made are undone. The fold is not offered again, even if its collision recurs: the file is then only withdrawn and pinned, so no eviction is made that a refusal would not undo. The layout settles again, and the symbol pass reruns, and a settle can offer new folds, so the loop runs until nothing is refused, capped by the file count. No file move and no symbol move is printed for the file.

With the wall up, collisions practically never arise. The identity guard keeps a same-named file out of an occupied folder, and each package level is drawn as a mirror of the manifests (ADR-17), so a folder is never carried into another package. A collision forced inside one package is still withdrawn and folds within that package. The rule is checked in both wall modes: every fixture's analysis is swept for a file move that lands on an existing file.

## 🔀 Alternatives considered

- **Drop the colliding move and say nothing.** Rejected because the reason for the move (the declarations belong with the occupant) is lost.
- **Rename the moved file (`lib_2.rs`).** Rejected because it invents a name no one chose and, for a crate root such as `lib.rs`, produces a file that is no longer a crate root.
- **Merge whole files in the report ("merge util/src/lib.rs into core/src/lib.rs").** Rejected because it adds a new relocation kind to the report and JSON schema, while symbol moves already say the same thing at the granularity readers act on.
- **Only revert the file to its home cluster.** Rejected: when the display levels carry the whole folder, the file is already home, the revert does nothing, and the collision recurs.
- **Exempt folds from the reach guards, since a fold nominates nothing.** Rejected: a fold still hands the file's dependants a new folder to depend on, which is exactly what the guards forbid, and the guards must hold on every path.
- **Evict newcomers to the occupant's cluster unconditionally.** Rejected: an unchecked eviction can put two same-named files in one folder or break the capacity and cycle vetoes, recreating the problem the fold exists to prevent.

## ⚖️ Consequences

Together with [ADR-18](../reporting/adr-18-move-endpoints-are-real-paths-under-see-through-roots.md), every printed endpoint can be followed: every `to` folder exists or is created, and no file lands on an existing file.

In `workspace-move-rust` with the wall lifted, util's `crates/util/src/lib.rs` is a crate root. It is neither moved nor folded; it stays where it is. A fold of an ordinary file that clears every veto is printed as symbol moves into the occupant.

A folded candidate scores somewhat less than the colliding one did, because the fold pays for every symbol it moves instead of carrying the file for free. A refused fold costs an extra settling round and symbol pass per refusal; the rounds are capped by the file count, not the number of folds.

Settling is bounded by the file count, not by a small constant. Each withdrawal round re-assembles the candidate tree once per inner round, so the worst case is quadratic in the file count per restart, and each refused fold repeats it. The bound is kept on purpose: every round withdraws at least one colliding move, so the loop only runs as long as collisions remain, and a constant cap could stop with a collision still printed, breaking the no-overwrite rule. In practice every fixture and the self-analysis settle in one or two rounds; if a large repository ever shows settling cost, the fix is cheaper collision detection, not a lower bound.
