// Formatting for orders/model data.
import type { OrdersModelData } from "./types";

export function formatOrdersModel(data: OrdersModelData): string {
  return `${data.id}:${data.weight}`;
}
