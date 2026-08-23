// Validation guard for search/model data.
import type { SearchModelData } from "./types";

export function isValidSearchModel(data: SearchModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
