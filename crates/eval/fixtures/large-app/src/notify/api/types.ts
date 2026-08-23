// Data contract for notify/api.
export interface NotifyApiData {
  id: string;
  weight: number;
}

export type NotifyApiKind = "primary" | "secondary";
