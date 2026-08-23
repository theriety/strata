// Helper built on the core of notify/api.
import { NotifyApiCore } from "./core";

export function assistApi(core: NotifyApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
