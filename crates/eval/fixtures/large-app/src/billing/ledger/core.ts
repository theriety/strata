// Core logic for billing/ledger.
import type { BillingLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class BillingLedgerCore {
  weigh(data: BillingLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: BillingLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
