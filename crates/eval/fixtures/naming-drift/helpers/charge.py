"""Card charging: payment logic, not a utility."""

_ledger = []


def charge_card(token: str, cents: int) -> str:
    receipt = "{}:{}".format(token, cents)
    _ledger.append(receipt)
    return receipt
