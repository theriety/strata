// Helper built on the core of search/ledger.
import { SearchLedgerCore } from "./core";

export function assistLedger(core: SearchLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
