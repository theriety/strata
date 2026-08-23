// Data contract for identity/api.
export interface IdentityApiData {
  id: string;
  weight: number;
}

export type IdentityApiKind = "primary" | "secondary";
