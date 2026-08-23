// Validation guard for identity/model data.
import type { IdentityModelData } from "./types";

export function isValidIdentityModel(data: IdentityModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
