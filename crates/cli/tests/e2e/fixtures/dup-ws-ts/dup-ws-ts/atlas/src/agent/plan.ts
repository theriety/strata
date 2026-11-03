import { alpha } from '../core/alpha';
import { delta } from '../core/delta';

export function plan(): number {
  return alpha() + delta();
}
