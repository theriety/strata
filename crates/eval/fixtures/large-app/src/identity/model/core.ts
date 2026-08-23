// Core logic for identity/model.
import type { IdentityModelData } from "./types";

export class IdentityModelCore {
  weigh(data: IdentityModelData): number {
    return data.weight * 8;
  }
}
