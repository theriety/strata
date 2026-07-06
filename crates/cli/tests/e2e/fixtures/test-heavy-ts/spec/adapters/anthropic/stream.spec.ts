import { sendMessage } from '../../../src/adapters/anthropic/client';
import { modelName } from '../../../src/adapters/anthropic/models';

export function shouldStreamMessages(): boolean {
  return sendMessage(modelName(), 'chunk').includes('->');
}
