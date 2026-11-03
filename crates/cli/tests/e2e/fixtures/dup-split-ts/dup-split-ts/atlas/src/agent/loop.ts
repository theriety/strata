import { alpha } from '../core/alpha';
import { beta } from '../core/beta';
import { gamma } from '../core/gamma';

export function loop(): number {
  return alpha() + beta() + gamma();
}
