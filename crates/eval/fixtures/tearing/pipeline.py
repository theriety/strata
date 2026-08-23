"""Order pipeline: the only bridge between billing and telemetry."""

from billing.invoice import issue_invoice
from telemetry.sink import emit


def process_order(order_id: str, units: int) -> str:
    invoice = issue_invoice(order_id, units)
    emit("ordered:{}".format(invoice))
    return invoice
