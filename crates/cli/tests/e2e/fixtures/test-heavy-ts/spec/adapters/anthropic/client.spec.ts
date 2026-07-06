import { createClient, sendMessage } from '../../../src/adapters/anthropic/client';

export function shouldCreateClient(): boolean {
  return sendMessage(createClient('k'), 'hi').length > 0;
}
