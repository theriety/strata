import { modelName, modelContext } from '../../../src/adapters/anthropic/models';

export function shouldNameTheModel(): boolean {
  return modelName().length > 0 && modelContext() > 0;
}
