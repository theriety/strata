import { modelName } from './models';

export function createClient(key: string): string {
  return `${key}:${modelName()}`;
}

export function sendMessage(client: string, body: string): string {
  return `${client}->${body}`;
}

export function closeClient(client: string): string {
  return `${client}:closed`;
}
