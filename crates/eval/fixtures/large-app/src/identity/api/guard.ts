// Validation guard for identity/api data.
import type { IdentityApiData } from "./types";

export function isValidIdentityApi(data: IdentityApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
