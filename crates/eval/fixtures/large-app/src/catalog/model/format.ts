// Formatting for catalog/model data.
import type { CatalogModelData } from "./types";

export function formatCatalogModel(data: CatalogModelData): string {
  return `${data.id}:${data.weight}`;
}
