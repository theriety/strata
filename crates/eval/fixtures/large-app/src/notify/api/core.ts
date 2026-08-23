// Core logic for notify/api.
import type { NotifyApiData } from "./types";

export class NotifyApiCore {
  weigh(data: NotifyApiData): number {
    return data.weight * 6;
  }
}
