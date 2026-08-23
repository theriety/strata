// Data contract for catalog/ledger.
export interface CatalogLedgerData {
  id: string;
  weight: number;
}

export type CatalogLedgerKind = "primary" | "secondary";
