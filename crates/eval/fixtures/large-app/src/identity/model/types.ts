// Data contract for identity/model.
export interface IdentityModelData {
  id: string;
  weight: number;
}

export type IdentityModelKind = "primary" | "secondary";
