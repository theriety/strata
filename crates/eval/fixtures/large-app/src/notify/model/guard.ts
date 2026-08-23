// Validation guard for notify/model data.
import type { NotifyModelData } from "./types";

export function isValidNotifyModel(data: NotifyModelData): boolean {
  return data.id.length > 0 && data.weight >= 0;
}
