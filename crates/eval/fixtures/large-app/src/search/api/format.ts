// Formatting for search/api data.
import type { SearchApiData } from "./types";

export function formatSearchApi(data: SearchApiData): string {
  return `${data.id}:${data.weight}`;
}
