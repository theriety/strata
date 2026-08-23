// Formatting for search/model data.
import type { SearchModelData } from "./types";

export function formatSearchModel(data: SearchModelData): string {
  return `${data.id}:${data.weight}`;
}
