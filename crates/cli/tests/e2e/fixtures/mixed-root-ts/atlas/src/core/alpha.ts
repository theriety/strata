import { beta } from './beta';
import { gamma } from './gamma';

export function alpha(): number {
  return beta() + gamma();
}
