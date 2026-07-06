import { alpha } from '../core/alpha';
import { beta } from '../core/beta';
import { clamp } from '../util/clamp';

export function gamma(value: number): number {
  return alpha(value) + beta(value) + clamp(value);
}
