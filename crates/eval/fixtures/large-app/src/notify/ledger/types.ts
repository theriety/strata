// Data contract for notify/ledger.
export interface NotifyLedgerData {
  id: string;
  weight: number;
}

export type NotifyLedgerKind = "primary" | "secondary";
