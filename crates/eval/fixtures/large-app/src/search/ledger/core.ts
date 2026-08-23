// Core logic for search/ledger.
import type { SearchLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class SearchLedgerCore {
  weigh(data: SearchLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: SearchLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
