// Helper built on the core of orders/model.
import { OrdersModelCore } from "./core";

export function assistModel(core: OrdersModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
