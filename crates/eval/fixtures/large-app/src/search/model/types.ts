// Data contract for search/model.
export interface SearchModelData {
  id: string;
  weight: number;
}

export type SearchModelKind = "primary" | "secondary";
