// Validation guard for billing/api data.
import type { BillingApiData } from "./types";

export function isValidBillingApi(data: BillingApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
