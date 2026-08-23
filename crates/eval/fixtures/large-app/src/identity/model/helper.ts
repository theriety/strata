// Helper built on the core of identity/model.
import { IdentityModelCore } from "./core";

export function assistModel(core: IdentityModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
