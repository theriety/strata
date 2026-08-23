// Data contract for identity/ledger.
export interface IdentityLedgerData {
  id: string;
  weight: number;
}

export type IdentityLedgerKind = "primary" | "secondary";
