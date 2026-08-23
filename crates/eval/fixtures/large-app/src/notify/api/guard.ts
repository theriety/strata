// Validation guard for notify/api data.
import type { NotifyApiData } from "./types";

export function isValidNotifyApi(data: NotifyApiData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
