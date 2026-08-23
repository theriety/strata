// Helper built on the core of billing/api.
import { BillingApiCore } from "./core";

export function assistApi(core: BillingApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
