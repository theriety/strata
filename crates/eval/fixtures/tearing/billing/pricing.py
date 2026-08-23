"""Price computation for the billing feature."""


def price_for(units: int) -> int:
    return units * 7


def discount_floor(units: int) -> int:
    return max(0, units - 3)
