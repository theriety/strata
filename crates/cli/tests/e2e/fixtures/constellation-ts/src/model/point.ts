import { shape } from '../model/shape';
import { lerp } from '../util/lerp';

export function point(value: number): number {
  return shape(value) + lerp(value);
}
