// Formatting for notify/model data.
import type { NotifyModelData } from "./types";

export function formatNotifyModel(data: NotifyModelData): string {
  return `${data.id}:${data.weight}`;
}
