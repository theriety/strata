import { logger } from '../util/logger';
import { buffer } from '../util/buffer';

export function reader(value: number): number {
  return logger(value) + buffer(value);
}
