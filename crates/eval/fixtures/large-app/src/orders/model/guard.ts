// Validation guard for orders/model data.
import type { OrdersModelData } from "./types";

export function isValidOrdersModel(data: OrdersModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
