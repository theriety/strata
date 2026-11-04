import { alpha } from '../atlas/src/core/alpha';

export function m(x: number): number {
  let total = 0;
  for (let i = 0; i < x; i += 1) {
    total += i * 2;
    if (total > 100) {
      total -= 7;
    }
  }
  return total + 1;
}

export function mAlpha(): number {
  return alpha() + m(3);
}
