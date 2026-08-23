// Core logic for orders/api.
import type { OrdersApiData } from "./types";

export class OrdersApiCore {
  weigh(data: OrdersApiData): number {
    return data.weight * 6;
  }
}
