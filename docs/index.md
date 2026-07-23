# strata documentation

Reference documentation for `strata`, a read-only static-analysis tool that
proposes a hierarchical, acyclic module decomposition for a codebase.

Start with the [README](../README.md) for what `strata` does and how to run it,
and [CONTRIBUTE](../CONTRIBUTE.md) for building, testing, and the gates.

## Architecture

- [Overview](architecture/overview.md) — crate layout, the two-stage
  adapter → IR snapshot → engine pipeline, the eleven-phase decomposition, the
  snapshot contract, and how anchored and greenfield modes differ.
- [Deviations](architecture/deviations.md) — where the shipped implementation
  intentionally departs from its original draft specs, and why.
