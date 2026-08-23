// Validation guard for identity/ledger data.
import type { IdentityLedgerData } from "./types";

export function isValidIdentityLedger(data: IdentityLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
