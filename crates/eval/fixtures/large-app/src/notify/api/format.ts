// Formatting for notify/api data.
import type { NotifyApiData } from "./types";

export function formatNotifyApi(data: NotifyApiData): string {
  return `${data.id}:${data.weight}`;
}
