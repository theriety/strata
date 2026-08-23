// Formatting for orders/api data.
import type { OrdersApiData } from "./types";

export function formatOrdersApi(data: OrdersApiData): string {
  return `${data.id}:${data.weight}`;
}
