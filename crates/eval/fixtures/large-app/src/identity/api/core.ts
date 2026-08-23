// Core logic for identity/api.
import type { IdentityApiData } from "./types";

export class IdentityApiCore {
  weigh(data: IdentityApiData): number {
    return data.weight * 8;
  }
}
