// Data contract for orders/model.
export interface OrdersModelData {
  id: string;
  weight: number;
}

export type OrdersModelKind = "primary" | "secondary";
