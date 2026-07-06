import { canvas } from '../render/canvas';

export function shouldCanvas(): boolean {
  return canvas(1) >= 0;
}
