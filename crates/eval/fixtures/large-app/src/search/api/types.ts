// Data contract for search/api.
export interface SearchApiData {
  id: string;
  weight: number;
}

export type SearchApiKind = "primary" | "secondary";
