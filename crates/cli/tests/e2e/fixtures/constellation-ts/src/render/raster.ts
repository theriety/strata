import { canvas } from '../render/canvas';
import { writer } from '../io/writer';

export function raster(value: number): number {
  return canvas(value) + writer(value);
}
