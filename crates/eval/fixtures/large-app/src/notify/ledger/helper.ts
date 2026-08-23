// Helper built on the core of notify/ledger.
import { NotifyLedgerCore } from "./core";

export function assistLedger(core: NotifyLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
