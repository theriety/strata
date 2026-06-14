import { run } from '../app';
import { makeRect } from './support';

export function shouldRun(): boolean {
  return run() === makeRect();
}
