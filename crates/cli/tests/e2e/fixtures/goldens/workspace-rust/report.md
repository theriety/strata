# Strata report

Snapshot `8795580f9da7539c0f81cfff9a5a94ff00821c1c6231cd0d0a6934cff6e2e7ac`.

- 14 symbols, 13 edges, 3 files

## Current layout

Score `0.7266`.

- cut `0.9211`, imbalance `0.0054`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

### Violations

- **Visibility** (Violation) at ReMeasure: `ReMeasure` is exported at Package but needed only at File
- **Visibility** (Violation) at Report: `Report` is exported at Package but needed only at File
- **Visibility** (Violation) at combined_total: `combined_total` is exported at Package but needed only at File
- **Visibility** (Violation) at describe: `describe` is exported at Package but needed only at File

## Anchored candidates

_Fewer than the requested candidates survived; the solution space converged._

_Current layout is already optimal; candidate 1 is the current tree._

### Candidate 1 (improvement `+0.0000`, score `0.7266`)

- cut `0.9211`, imbalance `0.0054`, naming `-0.0000`, path `-0.2000`, anchor `0.0000`

No moves versus the current layout.

## Greenfield candidates

_Fewer than the requested candidates survived; the solution space converged._

### Candidate 1 (improvement `+1.1014`, score `-0.1749`)

- cut `0.1197`, imbalance `0.0054`, naming `-0.3000`, path `-0.0000`, anchor `0.0000`

**Moves**

```
merge — pulled by lib.rs (w 7.8)
  1. crates/app/src/lib.rs [crates/app → crates/core]
  2. crates/util/src/lib.rs [crates/util → crates/core]
```

