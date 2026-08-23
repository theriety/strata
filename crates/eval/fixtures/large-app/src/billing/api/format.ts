// Formatting for billing/api data.
import type { BillingApiData } from "./types";

export function formatBillingApi(data: BillingApiData): string {
  return `${data.id}:${data.weight}`;
}
