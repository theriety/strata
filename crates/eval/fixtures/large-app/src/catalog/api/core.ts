// Core logic for catalog/api.
import type { CatalogApiData } from "./types";

export class CatalogApiCore {
  weigh(data: CatalogApiData): number {
    return data.weight * 7;
  }
}
