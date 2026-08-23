// Shelf holding kits: atlas package, store domain.
import { Kit } from "./kit";

export class Shelf {
  constructor(private readonly kits: Kit[]) {}

  count(): number {
    return this.kits.length;
  }
}
