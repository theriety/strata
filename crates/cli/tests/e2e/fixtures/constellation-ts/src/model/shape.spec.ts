import { shape } from '../model/shape';
import { point } from '../model/point';

export function shouldShape(): boolean {
  return shape(1) + point(1) >= 0;
}
