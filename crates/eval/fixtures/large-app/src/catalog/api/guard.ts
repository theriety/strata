// Validation guard for catalog/api data.
import type { CatalogApiData } from "./types";

export function isValidCatalogApi(data: CatalogApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
