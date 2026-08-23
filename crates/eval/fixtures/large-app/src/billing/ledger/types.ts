// Data contract for billing/ledger.
export interface BillingLedgerData {
  id: string;
  weight: number;
}

export type BillingLedgerKind = "primary" | "secondary";
