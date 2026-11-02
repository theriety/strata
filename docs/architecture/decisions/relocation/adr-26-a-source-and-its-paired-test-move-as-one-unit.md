# ADR-26: A source and its paired test move as one unit

📌

A source file and the test an adapter pairs with it either move together or not at all; an unpaired test stays inside its test root; an entry file never moves. The engine reads all three from adapter facts and never guesses from a path.

- Status: `Accepted`
- Date: `2026-10-04`

## 🎯 Motivation

[ADR-22](adr-22-test-symbols-are-pinned-by-polarity.md) let a source move even when its paired test could not follow, and reported the test as a blocked mirror. The result was a suggestion that is technically wrong: the source lands in a new folder while its test stays behind, so the pair no longer sits together. Unpaired tests could also be pulled into production folders, and an entry file such as `lib.rs` or `main.rs` could be proposed for a move that would break the build.

## 🧭 Context

Adapters already state which files are tests (`Node.test_source`, `FileFact.test_of`) and which are entries (`FileFact.entry`). The engine is language-neutral and holds no path convention of its own. Users decided on 2026-10-02: drop the whole move when a paired test cannot follow; treat unpaired tests as ordinary files whose moves stay inside the test folders; pin entry files.

## ✅ Decision

- **Paired tests move as a unit.** Every exact mirror is attempted for an accepted source move. If any follower is blocked (package boundary, namespace boundary, capacity or path collision), the whole move is dropped: the partition returns to its checkpoint, followers return home and the source returns to its pass-start folder. Collision settling ([ADR-21](adr-21-a-colliding-file-move-folds-into-symbol-moves.md)) runs another round after a dropped move.
- **Paired tests are always pinned** as standalone movers; they move only as followers. The `pin-detected-test-files` config key is removed; a configuration that still sets it is rejected as an unknown field.
- **Unpaired tests stay in their test root.** A test root is the topmost ancestor folder, within the same package, that holds only test-zone files. A test file's zone key is `<package>:<root>`, a production file's key is empty. The identity guard allows a join only when the mover's zones are a subset of the target cluster's allowed zones, so production never enters a test folder and an unpaired test never leaves its root, but may move inside it.
- **Entry files are pinned, and so is the program start.** Files whose adapter fact says `entry` never move, and the symbol an adapter names in `FileFact.start` (the true start function, set by the Rust adapter on the root `fn main` of every non-lib cargo target, build scripts included) is never moved out of its file by the symbol pass. The TypeScript and Python adapters set no start: a Python `if __name__ == "__main__":` block is not a symbol, and TypeScript has no start function. Other symbols in an entry file may still get symbol-move advice.
- **No `blockedMirrors`.** The result no longer carries blocked mirrors, since a blocked mirror now drops the move. The result schema stays at 10 for this stack.

## 🔀 Alternatives considered

**Keep best-effort followers.** Rejected: it produces suggestions that are wrong on their face.

**Detect test folders by name.** Rejected: it puts language conventions in the engine.

## ⚖️ Consequences

- Some moves that were suggested before disappear; this is intended.
- A pair that cannot move together stays put even when the source alone would score better.
- `matches_builtin_path` still marks conventional test-support paths as test zone; replacing it needs a new adapter test-support fact.
- Symbol moves out of entry files are not pinned by this decision.

- **Final pair check.** After the settle loop ends, whatever the exit, any pair whose source and test did not end together (one moved, or both moved apart) is reverted to its pass-start place, so the round cap cannot leave a split pair.
- **Mirroring off.** With mirroring off, a source may move without its paired test; strata then does not enforce pairs.
