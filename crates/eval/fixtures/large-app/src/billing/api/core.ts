// Core logic for billing/api.
import type { BillingApiData } from "./types";

export class BillingApiCore {
  weigh(data: BillingApiData): number {
    return data.weight * 7;
  }
}
