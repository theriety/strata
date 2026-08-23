// Core logic for catalog/ledger.
import type { CatalogLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class CatalogLedgerCore {
  weigh(data: CatalogLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: CatalogLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
