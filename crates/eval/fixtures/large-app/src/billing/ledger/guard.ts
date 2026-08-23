// Validation guard for billing/ledger data.
import type { BillingLedgerData } from "./types";

export function isValidBillingLedger(data: BillingLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
