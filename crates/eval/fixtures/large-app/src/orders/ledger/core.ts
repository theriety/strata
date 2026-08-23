// Core logic for orders/ledger.
import type { OrdersLedgerData } from "./types";
import type { ApiData } from "../api/types";

export class OrdersLedgerCore {
  weigh(data: OrdersLedgerData): number {
    return data.weight * 2;
  }

  reconcile(data: OrdersLedgerData, api: ApiData): boolean {
    return data.id === api.id;
  }
}
