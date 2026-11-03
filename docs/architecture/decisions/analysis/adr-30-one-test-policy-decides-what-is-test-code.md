# ADR-30: One test policy decides what is test code

📌

Unlace answers "is this file test code?" in exactly one place: an engine-level test policy that combines what each language adapter knows with the project's `[tests]` settings. Every part of the analysis reads that one answer, so no two parts can disagree about the same file.

- Status: `Accepted`
- Date: `2026-11-03`
- Supersedes: ADR-25 in part (its testOnly rule's reliance on the adapter's `Node.test_source` alone)

## 🎯 Motivation

A TypeScript repository keeps its test helpers in `spec/batch/utilities.ts`. Unlace reported every helper there as "Only tests in this repo use X", because the TypeScript adapter recognised tests only by `.spec.`/`.test.` names. Fixing the adapter was not enough. The relocation solver had its own path rule (any `spec`/`test`/`tests` folder at any depth), measured from any package manifest. The testOnly finding read the adapter's flag, measured from `package.json` only. A TypeScript helper inside a `Cargo.toml` directory was test code to one and production code to the other. Users who added `tests.patterns` or a `!` exclusion changed some consumers and not others: a negated file still had its symbols pinned and still triggered "production depends on test code".

Every consumer that answers the question itself drifts. The fix is to answer it once.

## 🧭 Context

Adapters state facts about their own language: whether a node is declared in test code (`Node.test_source`), the polarity they compute (production, test case, test support), source–test file pairs (`FileFact.test_of`), and test-support crates. Several engine consumers need a test verdict:

- the testOnly finding;
- the solver's test zones and tie-cut;
- the symbol pass's pinning (ADR-22);
- polarity findings;
- capacity line counts and scoring, which count production lines only.

`[tests]` settings are `builtins` (whether adapter detection counts), `patterns` (globs, with `!` for exclusion) and `helper-cap`.

Two analysis profiles (anchored and greenfield) score the same snapshot. The current-layout tree and the merged advice are shared by both.

## ✅ Decision

One engine `TestPolicy` (`crates/engine/src/analyze/test_policy.rs`) decides test code. It runs once per snapshot, right after the adapters' fragments merge, and re-labels node polarity so every consumer reads the same verdict.

1. **Precedence.** A `!` pattern wins over everything: the file is production. Otherwise, a positive pattern marks the file as test code. Otherwise, with `builtins = true`, the adapters' facts decide (test source, polarity, test pairs, test-support crates). With `builtins = false`, adapter facts count for nothing, test pairing included.
2. **Re-labelling works both ways.** Nodes in a `!`-matched file become production. Production nodes in a pattern-marked file become test support. Capacity, scoring, pinning and polarity findings all see the result.
3. **Language rules stay in adapters.** The engine never re-derives a language's test convention from path shape.
   - TypeScript and JavaScript: `.spec.`/`.test.` names, `__tests__` folders, and `spec`/`test`/`tests` as the first folder below any enclosing `package.json` directory or the analysed root. A fixture package nested inside another package's test folder therefore stays test code.
   - Python and Rust keep their existing rules.
4. **Project-level settings.** `[tests]` is one top-level section shared by both profiles, because what counts as a test is a fact about the code, not a scoring opinion. The result carries it once as a top-level `tests` field (result schema 11).
5. **Defaults change nothing.** With default settings and no patterns, the policy re-labels nothing.

## 🔀 Alternatives considered

**Keep per-consumer rules and keep them in sync.** Rejected. They had already drifted (manifest kinds, folder depth, negation coverage), and each new consumer would add another copy.

**Per-profile test settings with a per-profile snapshot view.** Rejected. Shared outputs (current tree, advice) can't follow two answers, and two profiles judging different code weakens the "both profiles agree" signal.

**Let `!` affect only the testOnly finding and the solver.** Rejected. "Not test code" must mean the same everywhere, or pinning and polarity findings contradict the user's own setting.

## 🤝 Accepted Trade Offs

- Re-labelling changes capacity and scores for repositories that use positive patterns. This buys one consistent meaning of "test code".
- Test settings can no longer differ per profile. This buys shared outputs that are correct for both profiles.

## ⚖️ Consequences

The solver and the testOnly finding can no longer disagree. A new consumer reads the policy instead of writing its own rule. The engine depends on adapters stating their built-in verdict faithfully: an adapter bug shows up everywhere at once, which makes it visible and fixable in one place.
