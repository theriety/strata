// Data contract for orders/ledger.
export interface OrdersLedgerData {
  id: string;
  weight: number;
}

export type OrdersLedgerKind = "primary" | "secondary";
