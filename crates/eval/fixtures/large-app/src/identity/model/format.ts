// Formatting for identity/model data.
import type { IdentityModelData } from "./types";

export function formatIdentityModel(data: IdentityModelData): string {
  return `${data.id}:${data.weight}`;
}
