// Validation guard for search/api data.
import type { SearchApiData } from "./types";

export function isValidSearchApi(data: SearchApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
