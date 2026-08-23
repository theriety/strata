// Formatting for catalog/ledger data.
import type { CatalogLedgerData } from "./types";

export function formatCatalogLedger(data: CatalogLedgerData): string {
  return `${data.id}:${data.weight}`;
}
