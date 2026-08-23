// Data contract for catalog/api.
export interface CatalogApiData {
  id: string;
  weight: number;
}

export type CatalogApiKind = "primary" | "secondary";
