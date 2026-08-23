// Core logic for orders/model.
import type { OrdersModelData } from "./types";

export class OrdersModelCore {
  weigh(data: OrdersModelData): number {
    return data.weight * 6;
  }
}
