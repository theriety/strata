"""Coupling tolerance table: no priced dependency on either side."""


def tolerance(bore_mm: float) -> float:
    return bore_mm * 0.01
