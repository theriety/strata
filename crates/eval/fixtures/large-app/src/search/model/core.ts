// Core logic for search/model.
import type { SearchModelData } from "./types";

export class SearchModelCore {
  weigh(data: SearchModelData): number {
    return data.weight * 6;
  }
}
