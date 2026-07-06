import { alpha } from '../core/alpha';
import { clamp } from '../util/clamp';

export function beta(value: number): number {
  return alpha(value) + clamp(value);
}
