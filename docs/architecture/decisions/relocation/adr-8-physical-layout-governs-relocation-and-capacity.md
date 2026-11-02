# ADR-8: Physical layout governs relocation and capacity

📌

Strata keeps semantic clustering separate from physical repository structure: move narration, namespace boundaries, relocation depth, and folder capacity all use pass-start physical paths, while normalized paths serve only clustering.

- Status: `Accepted`
- Date: `2026-08-29`

## 🎯 Motivation

Using normalized containers for move narration can hide path segments such as source and specification roots. Using them for capacity can merge physically separate folders or charge a folder for every descendant. Either result gives the reader an incorrect account of the proposed move or the folder that exceeds its budget.

Symbol relocation also needs a directional structural boundary. Moving a declaration from a shallower directory into a deeper one assigns a general declaration to a more specialized physical scope. Objective improvement alone is not sufficient evidence for that change in ownership.

## 🧭 Context

Strata maintains a normalized laminar tree—a hierarchy of nested semantic containers—for clustering related code. That structure may omit transparent path segments and may place files from distinct physical directories in the same semantic container. It is useful for finding related code, but it is not a faithful representation of repository paths or of the entries that occupy a physical folder.

## ✅ Decision

Strata keeps semantic clustering and physical repository structure as separate private projections. Normalized paths remain available for clustering. Every operation concerned with presentation, relocation depth, or folder occupancy uses the pass-start physical paths instead.

File-move narration identifies both endpoints with dataset-qualified physical paths. The moved leaf includes its complete repository-relative path, and the destination includes the dataset followed by its complete physical directory. Transparent path segments remain visible. A move between `src/core/item.ts` and `src/shared`, for example, is presented as a move of `dataset/src/core/item.ts` into `dataset/src/shared`.

Symbol relocation preserves an opaque pass-start namespace as a hard boundary. The namespace is derived privately from physical placement without recognizing specific directory names or application concepts. Files may join only destinations permitted for their namespace, and symbols may move only between files with the same namespace. Destination permissions are frozen at pass start, so earlier relocations cannot authorize later crossings.

Within a namespace, a runtime symbol or type declaration may not move to a physical destination directory deeper than its pass-start source directory. Depth is the number of directory segments in the repository-relative physical path. Equal-depth and upward moves remain eligible. This rule is a hard admission constraint evaluated before tentative placement and scoring.

Executable top-level content is represented as `NodeKind::FileBody`. `FileBody` retains its attributed source lines of code, dependency edges, and runtime role in file classification, but it cannot relocate independently. It moves only with its containing file, subject to the file's namespace boundary.

A physical folder's capacity usage is the sum of:

- files whose immediate parent is that folder; and
- distinct folders whose immediate parent is that folder.

Descendant entries below those immediate child folders do not contribute to the parent's usage. Physically distinct namespaces are never merged for this measure. The same definition governs capacity findings, current-tree infeasibility, candidate remainder, objective pressure, relief construction, move admission, and narration. Domain and other non-folder capacity measures retain their existing definitions.

Capacity remains part of the objective with the configured capacity coefficient and existing pricing formula. The decision changes the Folder measure, not whether capacity is priced or how the coefficient is configured.

These rules use private engine metadata. They add no public configuration, IR, result-schema, evaluation-corpus, or scoring-coefficient field.

## 🔀 Alternatives considered

**Use normalized containers for every concern.** Rejected because semantic proximity does not preserve physical path identity and can merge entries that occupy different repository folders.

**Treat transparent roots as presentation-only aliases.** Rejected because the same roots separate physical namespaces for relocation and capacity. Hiding them would make reported paths disagree with the structure being governed.

**Count all descendant files toward a folder budget.** Rejected because a folder cap governs one directory level. Recursive counting makes a well-factored subtree appear to occupy its parent directly and applies the same descendants to multiple ancestors.

**Count immediate files but not child folders.** Rejected because a child folder is itself an entry at that level and consumes organizational capacity.

**Price deeper symbol moves instead of rejecting them.** Rejected because a move into a more specialized scope changes ownership. That structural decision must not be traded against unrelated objective improvements.

**Infer generality from names or declaration kinds.** Rejected because names and application-specific conventions are weaker and less portable evidence than physical depth. The depth boundary applies equally to runtime symbols and types.

## ⚖️ Consequences

Move reports expose the physical source and destination a maintainer would edit, including transparent roots and the dataset boundary. Capacity findings identify one physical folder and describe only entries at that folder's level.

Normalized clustering can still relate code across transparent roots without granting permission to cross their namespace boundary or conflating their capacity. Pass-start namespace permissions and non-relocatable `FileBody` content remain structural invariants.

Some symbol moves that improve the objective are no longer candidates when they place a declaration deeper in the physical tree. Folder capacity scores and candidate ordering can change because the Folder pressure term now prices immediate physical entries. The capacity coefficient, non-Folder capacity semantics, and public interfaces remain unchanged.
