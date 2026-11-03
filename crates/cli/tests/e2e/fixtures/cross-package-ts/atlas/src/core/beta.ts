import { gamma } from './gamma';
import { delta } from './delta';

export function beta(): number {
  return gamma() + delta();
}
