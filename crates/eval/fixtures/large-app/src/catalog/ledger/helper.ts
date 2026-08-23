// Helper built on the core of catalog/ledger.
import { CatalogLedgerCore } from "./core";

export function assistLedger(core: CatalogLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
