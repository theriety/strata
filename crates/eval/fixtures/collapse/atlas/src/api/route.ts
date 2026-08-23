// API route over the store: atlas package.
import { Kit } from "../store/kit";

export function route(kit: Kit): string {
  return `/kits/${kit.part()}`;
}
