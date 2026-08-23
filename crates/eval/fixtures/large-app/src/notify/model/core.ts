// Core logic for notify/model.
import type { NotifyModelData } from "./types";

export class NotifyModelCore {
  weigh(data: NotifyModelData): number {
    return data.weight * 6;
  }
}
