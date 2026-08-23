"""Ledger persistence for the billing feature."""

from .pricing import discount_floor


def record_entry(order_id: str, total: int) -> str:
    adjusted = total - discount_floor(1)
    return "{}:{}".format(order_id, adjusted)
