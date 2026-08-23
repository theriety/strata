// Helper built on the core of search/api.
import { SearchApiCore } from "./core";

export function assistApi(core: SearchApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
