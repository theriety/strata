// Core logic for search/api.
import type { SearchApiData } from "./types";

export class SearchApiCore {
  weigh(data: SearchApiData): number {
    return data.weight * 6;
  }
}
