// Formatting for orders/ledger data.
import type { OrdersLedgerData } from "./types";

export function formatOrdersLedger(data: OrdersLedgerData): string {
  return `${data.id}:${data.weight}`;
}
