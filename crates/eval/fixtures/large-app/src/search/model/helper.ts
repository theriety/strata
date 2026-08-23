// Helper built on the core of search/model.
import { SearchModelCore } from "./core";

export function assistModel(core: SearchModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
