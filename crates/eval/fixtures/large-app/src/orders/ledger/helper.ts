// Helper built on the core of orders/ledger.
import { OrdersLedgerCore } from "./core";

export function assistLedger(core: OrdersLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
