// Core logic for catalog/model.
import type { CatalogModelData } from "./types";

export class CatalogModelCore {
  weigh(data: CatalogModelData): number {
    return data.weight * 7;
  }
}
