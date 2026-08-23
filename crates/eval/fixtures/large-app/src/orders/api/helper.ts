// Helper built on the core of orders/api.
import { OrdersApiCore } from "./core";

export function assistApi(core: OrdersApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
