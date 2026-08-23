// Helper built on the core of billing/ledger.
import { BillingLedgerCore } from "./core";

export function assistLedger(core: BillingLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
