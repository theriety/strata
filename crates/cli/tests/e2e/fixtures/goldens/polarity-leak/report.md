# Strata report

Snapshot `b15b386f3f11d23998f8eb73bb45713a2c46f57fe5cf6f00f06d525092634625`.

- 2 symbols, 1 edges, 2 files

## Current layout

Score `17.0000`.

- cut `16.0000`, imbalance `1.0000`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`

### Violations

- **Polarity** (Violation) at serve, sample_fixture: production symbol `serve` depends on test code `sample_fixture`

## Anchored candidates

### Candidate 1 (score `3.5000`)

- cut `2.0000`, imbalance `1.0000`, naming `-0.0000`, path `-0.0000`, anchor `0.5000`

**Moves**

- Move `service.py`: polarity-leak -> polarity-leak/conftest.py/conftest.py/conftest.py (cohesion gain)

## Greenfield candidates

### Candidate 1 (score `3.0000`)

- cut `2.0000`, imbalance `1.0000`, naming `-0.0000`, path `-0.0000`, anchor `0.0000`

**Moves**

- Move `service.py`: polarity-leak -> polarity-leak/conftest.py/conftest.py/conftest.py (cohesion gain)

