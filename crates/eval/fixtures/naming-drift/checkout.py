"""Checkout flow: the only consumer of the payment cluster."""

from helpers.charge import charge_card


def pay(token: str, cents: int) -> str:
    return charge_card(token, cents)
