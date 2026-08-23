// Validation guard for notify/ledger data.
import type { NotifyLedgerData } from "./types";

export function isValidNotifyLedger(data: NotifyLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
