import { complete } from '../api/openai';

export function shouldComplete(): boolean {
  return complete(' hi ') === 'hi';
}
