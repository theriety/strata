// Data contract for billing/model.
export interface BillingModelData {
  id: string;
  weight: number;
}

export type BillingModelKind = "primary" | "secondary";
