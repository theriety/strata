// Helper built on the core of notify/model.
import { NotifyModelCore } from "./core";

export function assistModel(core: NotifyModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
