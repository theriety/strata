import { Rectangle } from './geometry/rectangle';

export function run(): number {
  const rect = new Rectangle({ width: 2, height: 3 });
  return rect.area();
}

export async function lazy(): Promise<number> {
  const mod = await import('./geometry/rectangle');
  return new mod.Rectangle({ width: 1, height: 1 }).area();
}
