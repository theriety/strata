import { Shape } from './shape';
import type { Dimensions } from './shape';

/**
 * An axis-aligned rectangle.
 */
export class Rectangle implements Shape {
  constructor(private readonly size: Dimensions) {}

  area(): number {
    return this.size.width * this.size.height;
  }
}

export function describe(size: Dimensions): string {
  // build a label lazily
  return `${size.width}x${size.height}`;
}
