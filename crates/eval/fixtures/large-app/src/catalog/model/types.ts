// Data contract for catalog/model.
export interface CatalogModelData {
  id: string;
  weight: number;
}

export type CatalogModelKind = "primary" | "secondary";
