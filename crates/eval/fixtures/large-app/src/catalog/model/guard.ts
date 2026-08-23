// Validation guard for catalog/model data.
import type { CatalogModelData } from "./types";

export function isValidCatalogModel(data: CatalogModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
