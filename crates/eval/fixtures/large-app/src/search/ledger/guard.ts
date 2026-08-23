// Validation guard for search/ledger data.
import type { SearchLedgerData } from "./types";

export function isValidSearchLedger(data: SearchLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
