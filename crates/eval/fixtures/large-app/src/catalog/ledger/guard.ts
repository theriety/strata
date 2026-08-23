// Validation guard for catalog/ledger data.
import type { CatalogLedgerData } from "./types";

export function isValidCatalogLedger(data: CatalogLedgerData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
