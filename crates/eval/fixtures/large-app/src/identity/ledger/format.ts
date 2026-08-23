// Formatting for identity/ledger data.
import type { IdentityLedgerData } from "./types";

export function formatIdentityLedger(data: IdentityLedgerData): string {
  return `${data.id}:${data.weight}`;
}
