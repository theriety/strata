import { shader } from '../render/shader';
import { buffer } from '../util/buffer';

export function texture(value: number): number {
  return shader(value) + buffer(value);
}
