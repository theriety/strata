// Data contract for search/ledger.
export interface SearchLedgerData {
  id: string;
  weight: number;
}

export type SearchLedgerKind = "primary" | "secondary";
