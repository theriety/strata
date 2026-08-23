// Formatting for search/ledger data.
import type { SearchLedgerData } from "./types";

export function formatSearchLedger(data: SearchLedgerData): string {
  return `${data.id}:${data.weight}`;
}
