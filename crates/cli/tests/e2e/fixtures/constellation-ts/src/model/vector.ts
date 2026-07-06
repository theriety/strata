import { point } from '../model/point';
import { beta } from '../core/beta';
import { lerp } from '../util/lerp';

export function vector(value: number): number {
  return point(value) + beta(value) + lerp(value);
}
