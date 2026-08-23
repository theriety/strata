// Validation guard for orders/api data.
import type { OrdersApiData } from "./types";

export function isValidOrdersApi(data: OrdersApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
