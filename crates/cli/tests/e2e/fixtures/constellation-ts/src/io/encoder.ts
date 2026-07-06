import { writer } from '../io/writer';
import { buffer } from '../util/buffer';

export function encoder(value: number): number {
  return writer(value) + buffer(value);
}
