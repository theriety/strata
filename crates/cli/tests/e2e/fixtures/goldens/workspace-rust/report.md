# Strata report

Snapshot `8795580f9da7539c0f81cfff9a5a94ff00821c1c6231cd0d0a6934cff6e2e7ac`.

- 14 symbols, 13 edges, 3 files

## Current layout

Score `157.5054`.

- cut `157.7000`, imbalance `0.0054`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

### Violations

- **Visibility** (Violation) at ReMeasure: `ReMeasure` is exported at Package but needed only at File
- **Visibility** (Violation) at Report: `Report` is exported at Package but needed only at File
- **Visibility** (Violation) at combined_total: `combined_total` is exported at Package but needed only at File
- **Visibility** (Violation) at describe: `describe` is exported at Package but needed only at File

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+136.8865`, score `20.6189`)

- cut `20.5000`, imbalance `0.0054`, naming `-0.3000`, path `-0.0865`, anchor `0.5000`

**Moves**

```
merge — pulled by lib.rs (w 7.8)
  1. crates/app/src/lib.rs [crates/app → crates/core]
  2. crates/util/src/lib.rs [crates/util → crates/core]
```

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+137.5000`, score `20.2054`)

- cut `20.5000`, imbalance `0.0054`, naming `-0.3000`, path `-0.0000`, anchor `0.0000`

**Moves**

```
merge — pulled by lib.rs (w 6.0)
  1. crates/core/src/lib.rs [crates/core → crates/app]
  2. crates/util/src/lib.rs [crates/util → crates/app]
```

