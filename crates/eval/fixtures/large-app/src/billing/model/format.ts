// Formatting for billing/model data.
import type { BillingModelData } from "./types";

export function formatBillingModel(data: BillingModelData): string {
  return `${data.id}:${data.weight}`;
}
