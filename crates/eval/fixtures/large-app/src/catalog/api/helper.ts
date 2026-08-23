// Helper built on the core of catalog/api.
import { CatalogApiCore } from "./core";

export function assistApi(core: CatalogApiCore, weight: number): number {
  return core.weigh({ id: "assist", weight });
}
