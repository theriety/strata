// Data contract for billing/api.
export interface BillingApiData {
  id: string;
  weight: number;
}

export type BillingApiKind = "primary" | "secondary";
