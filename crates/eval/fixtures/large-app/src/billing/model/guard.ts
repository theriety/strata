// Validation guard for billing/model data.
import type { BillingModelData } from "./types";

export function isValidBillingModel(data: BillingModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
