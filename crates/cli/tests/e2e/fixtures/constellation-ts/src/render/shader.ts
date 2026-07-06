import { canvas } from '../render/canvas';
import { stream } from '../io/stream';

export function shader(value: number): number {
  return canvas(value) + stream(value);
}
