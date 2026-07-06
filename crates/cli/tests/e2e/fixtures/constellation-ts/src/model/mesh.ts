import { matrix } from '../model/matrix';
import { shape } from '../model/shape';
import { delta } from '../core/delta';
import { hash } from '../util/hash';

export function mesh(value: number): number {
  return matrix(value) + shape(value) + delta(value) + hash(value);
}
