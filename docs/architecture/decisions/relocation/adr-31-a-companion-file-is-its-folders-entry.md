# ADR-31: A companion file is its folder's entry

📌

A TypeScript module `x.ts` that sits beside a folder `x/` is that folder's entry, exactly as if it were `x/index.ts`. The adapter states the fact as `FileFact::folder_companion`; the engine then treats the module as part of the folder everywhere it reasons about structure. Reports keep physical paths.

- Status: `Accepted`
- Date: `2026-11-24`
- Amends: ADR-8 (physical layout governs relocation and capacity)

## 🎯 Motivation

Many TypeScript repositories name a folder's entry after the folder and keep it beside the folder rather than inside it: `src/lexer/block.ts` next to `src/lexer/block/`. On mdc, seven such pairs exist and none of the folders has an `index.ts`. Unlace sees `block.ts` as a sibling of the folder, so it reads the folder's contents as unrelated to their own entry. It proposes hoisting the folder's files into `src/lexer/`, moving the entry's code into the folder, and flagging `block.ts` as a misplaced file. These are false moves: the layout is deliberate, and it is the same layout as `block/index.ts` with the entry written one level up.

ADR-8 makes the pass-start physical path the unit for capacity, depth and narration. The path alone cannot say that `block.ts` and `block/` belong together, and no language-neutral rule can. Whether a file is a folder's entry is a language convention, so the adapter that owns the language has to state it.

## 🧭 Context

- ADR-8 keeps semantic clustering and physical layout apart. It adds no IR field, so the engine has no way to learn that two physical entries are one unit.
- ADR-26 already moves a source and its paired test as one unit, driven by an adapter-stated `FileFact`. A companion is the same kind of knowledge.
- The adapter's own container tree is discarded when snapshots merge: the engine rebuilds it from the merged facts. A fact is the only channel that survives.

## ✅ Decision

1. The TypeScript adapter states `FileFact::folder_companion = "<dir>/x"` for a module `<dir>/x.<ext>` when:
   - `<dir>/x/` holds at least one analysed TypeScript-family module, at any depth;
   - `<dir>/x/index.<ext>` does not exist, so the folder has no entry of its own; and
   - exactly one module has the stem `<dir>/x`. If `x.ts` and `x.tsx` both exist, the match is ambiguous and it states nothing.
2. A paired test carries the mirrored companion. If `spec/lexer/block.spec.ts` is the test of `src/lexer/block.ts` and that source has companion `src/lexer/block`, the test's fact carries `spec/lexer/block`, but only when that folder holds analysed modules. Otherwise the test has none. This follows ADR-26: a source and its paired test move as one unit.
3. A file's facts stay one `FileFact` per file. The companion shares the record with the test pair when both apply.
4. The engine treats `x.ts` as `x/index.ts` everywhere it reasons about structure: clustering, cut, reach and alternatives, imbalance, capacity, and the depth rule. A companion counts as the folder's entry, not as a sibling entry of the parent. Reports, narration and moves keep the physical paths (ADR-8). The engine side is specified and delivered separately from this fact.
5. When unsure, state nothing. A missing fact means "not a companion".
6. Rust and Python adapters state no companions. Their module conventions (`mod.rs`, `__init__.py`) already place the entry inside the folder.

This amends ADR-8. Its statement that the physical-layout rules "add no public configuration, IR, result-schema, evaluation-corpus, or scoring-coefficient field" no longer holds for the IR: `FileFact` gains one optional field, `folder_companion`, omitted from serialized snapshots when absent. ADR-8's physical paths, namespace boundary, depth rule and capacity measure stay as written. They now apply to a companion as if it sat inside its folder.

## 🔀 Alternatives considered

- **An affinity record between `x.ts` and the folder's files.** Rejected: an affinity only pulls the files together in clustering. It does not make the file the folder's entry for capacity, the depth rule or imbalance, so those would still see a sibling.
- **Edit the adapter's container tree to nest `x.ts` under `x/`.** Rejected: the engine discards that tree at snapshot merge and rebuilds it with `build_laminar_tree`, so the edit would never reach the analysis.
- **Make the equivalence analysis-only, leaving capacity, depth and moves on raw paths.** Rejected by the user (decision E1): a file that clusters as the folder's entry but is charged and bounded as a sibling gives contradictory results.
- **Infer companions in the engine from file names.** Rejected: the convention is a TypeScript one, and the engine stays language-neutral.

## ⚖️ Consequences

- `FileFact` carries a companion, and the TypeScript adapter emits it from `pairs.rs` next to the test-pair facts. Snapshots without the fact are unchanged.
- Until the engine reads the fact, results do not change.
- A repository that names a folder's entry beside the folder gets stable, correct placement for it, and a paired spec follows when its own mirror folder exists.
- `x/` with only non-module assets, or a repository where both `x.ts` and `x.tsx` exist, behaves as before.
- A folder whose `index` file exists keeps that file as its entry. A stray `x.ts` beside it is not a second entry.
