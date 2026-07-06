import { vector } from '../model/vector';
import { gamma } from '../core/gamma';
import { hash } from '../util/hash';

export function matrix(value: number): number {
  return vector(value) + gamma(value) + hash(value);
}
