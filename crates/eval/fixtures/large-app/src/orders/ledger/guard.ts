// Validation guard for orders/ledger data.
import type { OrdersLedgerData } from "./types";

export function isValidOrdersLedger(data: OrdersLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
