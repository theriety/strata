import { alpha } from '../../../atlas/src/core/alpha';
import { beta } from '../../../atlas/src/core/beta';
import { gamma } from '../../../atlas/src/core/gamma';
import { delta } from '../../../atlas/src/core/delta';

export function stray(): number {
  return alpha() + beta() + gamma() + delta();
}
