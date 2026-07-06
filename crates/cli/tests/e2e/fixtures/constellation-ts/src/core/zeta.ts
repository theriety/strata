import { delta } from '../core/delta';
import { gamma } from '../core/gamma';

export function zeta(value: number): number {
  return delta(value) + gamma(value);
}
