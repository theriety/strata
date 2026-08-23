// Helper built on the core of billing/model.
import { BillingModelCore } from "./core";

export function assistModel(core: BillingModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
