# Eval fixture corpus

Synthetic repos, one structural failure mode each, consumed with their
constraint targets under `crates/eval/targets/` (schema and evaluation rules:
`crates/eval/targets/CONTRACT.md`). Owner-locked: synthetic content only, no
real OSS code, deterministic bytes, no timestamps or randomness.

| fixture | failure mode | language | files | the question |
| --- | --- | --- | --- | --- |
| `tearing` | directory-tearing | python | 6 | does a proposal keep a cohesive directory in one piece? |
| `welding` | cross-dir-welding | typescript | 5 | do zero-priced re-export barrels weld unrelated domains? |
| `relief` | over-capacity | python | 25 | does relieving an over-cap folder register in the proposal at all? |
| `collapse` | workspace-collapse | typescript | 5 src + 2 manifests | does a synthetic workspace bucket swallow real directories? |
| `inversion` | anchored-inversion | python | 7 | does anchored ever propose more change than greenfield on an optimal layout? |
| `naming-drift` | naming-incoherence | python | 5 | does a proposal regroup misnamed containers so names match contents? |
| `large-app` | scale | typescript | 108 | does proposal quality hold at three figures of file count? |
| `cycle-span` | import-cycle | python | 5 | does an import cycle spanning two real directories keep both whole without churning a healthy layout? |

Authoring rules live in CONTRACT.md; the short form: match the e2e fixture
style under `crates/cli/tests/e2e/fixtures/`, tiny files whose imports express
exactly the intended dependency graph, no `Cargo.toml` inside any fixture, a
`package.json` only when package-boundary semantics matter.

Provenance: `large-app/` is emitted by a deterministic generator (bytes are a
pure function of domain/module/kind; 6 domains x 3 modules x 6 kinds). The
generator is deliberately not committed — regenerate from the pattern documented
here if the tree is ever damaged: for every `<domain>` in
[billing, catalog, identity, notify, orders, search] and every `<module>` in
[api, ledger, model], emit `src/<domain>/<module>/{types,core,helper,format,
guard,index}.ts` where core holds one class over the module data type, ledger's
core additionally imports `../api/types`, helper wraps core, format/guard
consume types, index re-exports all five. Every other fixture is hand-written.

Status against current strata (2026-08-22, defaults): welding, collapse, and
large-app pass their targets outright (regression guards); tearing, relief,
inversion, and naming-drift carry failing assertions that pin today's measured
distance — see `.state/works/product-perfection/crp01-hand-validation.md`.
After the independent CRP03 checkability review, four targets gained coverage
guards (telemetry survival, chain-adjacency, barrel absorption, greenfield
churn) and CONTRACT.md gained the review's implementability fixes; the new
assertions' live verdicts are recorded in
`.state/works/product-perfection/crp03-checkability.md`.
