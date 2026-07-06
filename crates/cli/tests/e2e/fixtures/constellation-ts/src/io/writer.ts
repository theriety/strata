import { reader } from '../io/reader';
import { logger } from '../util/logger';

export function writer(value: number): number {
  return reader(value) + logger(value);
}
