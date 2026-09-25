# ADR-17: Relocations stay inside their package

📌

By default no file, symbol, test-mirror, or new-folder relocation crosses a package boundary; the wall is a hard admission rule inside the search, and each profile can lift it deliberately.

- Status: `Accepted`
- Date: `2026-09-25`

## 🎯 Motivation

Self-analysis of this repository proposed moving `crates/core/src/diversify.rs` and `crates/core/src/project.rs` into the engine crate's analysis cluster, and symbol moves such as `intern` from `adapter-python` into `ir`. Each of these changes a package's public surface, its manifest dependencies, and, for Rust, which crate may implement which trait. Those are release and ownership decisions, not layout improvements, and the objective cannot see their cost.

Filtering such moves out of the report after the search would leave candidate scores that count moves the user never sees.

## 🧭 Context

Strata already recognizes packages. Manifest-based package detection builds `ScopeLevel::Package` containers in the laminar tree, and every file knows its package through its laminar home. Relocation did not use that knowledge.

## ✅ Decision

By default, no relocation crosses a package boundary. A package is the existing `ScopeLevel::Package` container of a file's pass-start laminar home. Strata adds no new package detection. Files with no enclosing package container belong to one implicit root package.

With the wall up, a package level of the candidate tree is an uncapped mirror of the manifests: the capacity caps never split a real package, and each file of a folder is drawn under its own manifest package, including the files of a cross-package cycle.

The package wall is a hard admission constraint inside the search. It is not a scoring term and not an output filter. It covers every kind of relocation:

- **File moves.** A file may join a cluster only when its files lie in one package and every file physically in that cluster at pass start is in that package.
- **Cross-package cycles.** Files that import each other across packages condense into one strongly connected unit whose files span packages. Such a unit never moves and stays home: it stays in its pass-start cluster, joins no new group folder, and each of its files keeps its own package. The folder it sits in admits a newcomer only from the package of the files that physically sit in that folder; cycle members whose retained home is another package's folder do not widen what the folder admits.
- **Symbol moves.** A symbol may move only between files in the same package.
- **Test-mirror followers.** A follower whose destination would lie in a different package from its own pass-start package is blocked. It is reported as a blocked mirror with the new reason `packageBoundary`, which is checked before `namespaceBoundary`. As [ADR-22](adr-22-test-symbols-are-pinned-by-polarity.md) requires, a blocked follower does not veto its source move.
- **New group folders.** A folder created by a candidate must sit inside the package of every file it holds.

Package permissions are frozen at pass start, like the namespace permissions of [ADR-8](adr-8-physical-layout-governs-relocation-and-capacity.md). An accepted move cannot change which package a later move sees. Final candidates are checked against the same invariant.

### Opt-in switch

Each parameter profile owns the rule through its relocation policy:

```toml
[profiles.anchored.relocation]
allow-cross-package-moves = false   # default
```

The `analyze` command accepts `--allow-cross-package-moves`. The flag enables cross-package moves for every profile in that run, whatever the configuration says. Without the flag, each profile uses its own key. Effective configuration output reports the resolved value per profile.

The switch lifts only the package wall. Every other admission rule still applies, including the namespace and depth rules of ADR-8. Turning it on can therefore still produce no cross-package move.

## 🔀 Alternatives considered

**Filter cross-package moves from the output.** Rejected because the search would still score and select candidates containing moves that are never shown, so reported scores would describe an unreachable layout.

**A hard rule with no switch.** Rejected by the owner in favor of leaving room for deliberate monorepo refactors, provided the default stays closed.

**One global key.** Rejected because each profile already owns its relocation policy; a global key would force both profiles to agree.

**A new package detector for relocation.** Rejected because W0 manifest-based package containers already exist and a second detector could disagree with them.

**A score penalty for crossing.** Rejected because a package boundary is an ownership invariant and must not be traded against objective improvement.

## ⚖️ Consequences

Candidates describe layouts a maintainer can reach without changing any manifest or public crate surface. Some previously proposed moves disappear, so candidate scores and orderings can get worse and golden fixtures move.

Each profile's `parameters.relocation` in a saved result now carries `allow-cross-package-moves`. Relocation parameters reject unknown fields, so an older binary cannot read a result that contains the key. The result schema version therefore moves from 8 to 9. Readers accept only version 9 and reject version 8 and earlier with the standard schema error. The new `packageBoundary` blocked-mirror reason ships in the same version. A new configuration key and CLI flag need documentation in `README.md` and `strata.toml`, plus tests for the default, the per-profile key, and the flag overriding the key.

With the switch on, Strata does not model Rust's orphan rule. A cross-crate move of a trait implementation can then produce code that does not compile; the default keeps that case unreachable.
