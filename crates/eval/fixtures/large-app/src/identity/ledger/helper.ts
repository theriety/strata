// Helper built on the core of identity/ledger.
import { IdentityLedgerCore } from "./core";

export function assistLedger(core: IdentityLedgerCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
