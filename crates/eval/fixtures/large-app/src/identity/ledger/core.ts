// Core logic for identity/ledger.
import type { IdentityLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class IdentityLedgerCore {
  weigh(data: IdentityLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: IdentityLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
