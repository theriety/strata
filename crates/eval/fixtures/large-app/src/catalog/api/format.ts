// Formatting for catalog/api data.
import type { CatalogApiData } from "./types";

export function formatCatalogApi(data: CatalogApiData): string {
  return `${data.id}:${data.weight}`;
}
