import { beta } from './beta';
import { gamma } from './gamma';
import { m } from '../../../a/m';

export function alpha(): number {
  return beta() + gamma() + m(1);
}
