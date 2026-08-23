// Formatting for billing/ledger data.
import type { BillingLedgerData } from "./types";

export function formatBillingLedger(data: BillingLedgerData): string {
  return `${data.id}:${data.weight}`;
}
