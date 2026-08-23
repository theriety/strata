// Data contract for orders/api.
export interface OrdersApiData {
  id: string;
  weight: number;
}

export type OrdersApiKind = "primary" | "secondary";
