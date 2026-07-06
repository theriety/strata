import { shader } from '../render/shader';
import { canvas } from '../render/canvas';

export function epsilon(value: number): number {
  return shader(value) + canvas(value);
}
