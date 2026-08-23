// Helper built on the core of catalog/model.
import { CatalogModelCore } from "./core";

export function assistModel(core: CatalogModelCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
