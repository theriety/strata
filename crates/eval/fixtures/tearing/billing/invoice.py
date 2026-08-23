"""Invoice issuance for the billing feature."""

from .ledger import record_entry
from .pricing import price_for


def issue_invoice(order_id: str, units: int) -> str:
    total = price_for(units)
    record_entry(order_id, total)
    return "invoice-{}-{}".format(order_id, total)
