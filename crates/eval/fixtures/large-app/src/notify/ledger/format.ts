// Formatting for notify/ledger data.
import type { NotifyLedgerData } from "./types";

export function formatNotifyLedger(data: NotifyLedgerData): string {
  return `${data.id}:${data.weight}`;
}
