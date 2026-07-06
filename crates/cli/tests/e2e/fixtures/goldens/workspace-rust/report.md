# Strata report

Snapshot `6f6eff93efab8376097bc1ddc50dc68dc39b4e1092ef209226d1b1190ab25233`.

- 14 symbols, 13 edges, 3 files

## Current layout

Score `79.1054`.

- cut `79.3000`, imbalance `0.0054`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

### Violations

- **Visibility** (Violation) at ReMeasure: `ReMeasure` is exported at Package but needed only at File
- **Visibility** (Violation) at Report: `Report` is exported at Package but needed only at File
- **Visibility** (Violation) at combined_total: `combined_total` is exported at Package but needed only at File
- **Visibility** (Violation) at describe: `describe` is exported at Package but needed only at File

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+58.4865`, score `20.6189`)

- cut `20.5000`, imbalance `0.0054`, naming `-0.3000`, path `-0.0865`, anchor `0.5000`

**Moves**

- Merge `crates/app/src/lib.rs, crates/util/src/lib.rs`: workspace-rust/crates/app/src, workspace-rust/crates/util/src -> workspace-rust/crates/core/src (pulled by lib.rs (w 7.8))

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+59.1000`, score `20.2054`)

- cut `20.5000`, imbalance `0.0054`, naming `-0.3000`, path `-0.0000`, anchor `0.0000`

**Moves**

- Merge `crates/app/src/lib.rs, crates/util/src/lib.rs`: workspace-rust/crates/app/src, workspace-rust/crates/util/src -> workspace-rust/crates/core/src (pulled by lib.rs (w 7.8))

