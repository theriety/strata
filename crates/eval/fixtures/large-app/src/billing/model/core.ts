// Core logic for billing/model.
import type { BillingModelData } from "./types";

export class BillingModelCore {
  weigh(data: BillingModelData): number {
    return data.weight * 7;
  }
}
