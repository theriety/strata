import { ParseError } from '#errors';

/** runtime record after validating the construct discriminator */
export interface NodeRecord extends Record<string, unknown> {
  type: string;
}

/**
 * validates plain node records before reading their properties
 * @param value external node
 */
export function checkRecord(value: unknown): asserts value is NodeRecord {
  if (!isRecord(value)) {
    fail('Each node must be an object.');
  }

  const prototype: unknown = Object.getPrototypeOf(value);

  if (prototype !== Object.prototype && prototype !== null) {
    fail('Nodes must be plain objects.');
  }

  for (const key of Reflect.ownKeys(value)) {
    const descriptor = Object.getOwnPropertyDescriptor(value, key);

    if (
      typeof key !== 'string' ||
      !descriptor ||
      !('value' in descriptor) ||
      !descriptor.enumerable
    ) {
      fail('Nodes must have enumerable string data properties.');
    }
  }

  if (
    typeof value.type !== 'string' ||
    !/^[A-Za-z][A-Za-z0-9_-]*$/.test(value.type)
  ) {
    fail('Node type must be a selector identifier.');
  }
}

/**
 * raises an invalid-tree diagnostic
 * @param message actionable validation explanation
 */
export function fail(message: string): never {
  throw new ParseError('MDC_AST_INVALID', message);
}

/**
 * identifies non-null, non-array objects
 * @param value candidate object
 * @returns whether the value is an object record
 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
