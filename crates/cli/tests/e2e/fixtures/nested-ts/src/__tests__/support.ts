import { Rectangle } from '../geometry/rectangle';

export function makeRect(): number {
  return new Rectangle({ width: 2, height: 3 }).area();
}
