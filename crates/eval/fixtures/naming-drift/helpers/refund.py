"""Card refunds over issued charges."""

from .charge import _ledger


def refund_card(receipt: str) -> bool:
    return receipt in _ledger
