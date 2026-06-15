# Strata report

Snapshot `6f6eff93efab8376097bc1ddc50dc68dc39b4e1092ef209226d1b1190ab25233`.

- 14 symbols, 13 edges, 3 files

## Current layout

Score `79.3541`.

- cut `79.3000`, imbalance `0.0541`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`

### Violations

- **Visibility** (Violation) at describe: `describe` is exported at Package but needed only at File
- **Visibility** (Violation) at combined_total: `combined_total` is exported at Package but needed only at File
- **Visibility** (Violation) at Report: `Report` is exported at Package but needed only at File
- **Visibility** (Violation) at ReMeasure: `ReMeasure` is exported at Package but needed only at File

## Anchored candidates

### Candidate 1 (score `21.3398`)

- cut `20.5000`, imbalance `0.0541`, naming `-0.0000`, path `-0.0000`, anchor `0.7857`

**Moves**

- Move `crates/core/src/lib.rs, crates/util/src/lib.rs`: workspace-rust/crates -> workspace-rust/crates/crates/app/crates/app/src (cohesion gain)

## Greenfield candidates

### Candidate 1 (score `20.5541`)

- cut `20.5000`, imbalance `0.0541`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`

**Moves**

- Move `crates/core/src/lib.rs, crates/util/src/lib.rs`: workspace-rust/crates -> workspace-rust/crates/crates/app/crates/app/src (cohesion gain)

