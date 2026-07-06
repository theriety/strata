import { reader } from '../io/reader';
import { encoder } from '../io/encoder';

export function stream(value: number): number {
  return reader(value) + encoder(value);
}
