import { createClient, closeClient } from '../../../src/adapters/anthropic/client';

export function shouldRetryTheClient(): boolean {
  return closeClient(createClient('k')).endsWith('closed');
}
