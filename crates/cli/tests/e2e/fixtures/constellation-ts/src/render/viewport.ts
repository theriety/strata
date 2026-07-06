import { raster } from '../render/raster';
import { texture } from '../render/texture';
import { epsilon } from '../core/epsilon';

export function viewport(value: number): number {
  return raster(value) + texture(value) + epsilon(value);
}
