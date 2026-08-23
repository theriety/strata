// Core logic for notify/ledger.
import type { NotifyLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class NotifyLedgerCore {
  weigh(data: NotifyLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: NotifyLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
