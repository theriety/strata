import { beta } from '../core/beta';
import { lerp } from '../util/lerp';

export function delta(value: number): number {
  return beta(value) + lerp(value);
}
