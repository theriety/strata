// Helper built on the core of identity/api.
import { IdentityApiCore } from "./core";

export function assistApi(core: IdentityApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
