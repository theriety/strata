import { point } from '../model/point';
import { shape } from '../model/shape';
import { vector } from '../model/vector';

export function parser(value: number): number {
  return point(value) + shape(value) + vector(value);
}
