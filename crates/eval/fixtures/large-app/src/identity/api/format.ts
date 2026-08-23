// Formatting for identity/api data.
import type { IdentityApiData } from "./types";

export function formatIdentityApi(data: IdentityApiData): string {
  return `${data.id}:${data.weight}`;
}
