import { isDeepStrictEqual } from 'node:util';

import { parse } from '#parse';

import type { MdcxNode, MdcxFlowContent } from '#types';

import type { WriterContext } from './types';

/**
 * checks whether persistent metadata requires an annotation envelope
 * @param node semantic node
 * @returns whether namespace or metadata needs source representation
 */
export function hasMetadata(node: MdcxNode<string>): boolean {
  return (
    (node.kind ?? 'common') !== 'common' ||
    node.ref !== undefined ||
    Object.keys(node.attributes ?? {}).length > 0
  );
}

/**
 * checks only metadata selected for emission
 * @param node semantic node
 * @param context inclusion policy
 * @returns whether the selected envelope has content
 */
export function hasSelectedMetadata(
  node: MdcxNode<string>,
  context: WriterContext,
): boolean {
  return (
    context.includes.includes('type') ||
    (context.includes.includes('reference') && node.ref !== undefined) ||
    (context.includes.includes('attributes') &&
      Object.keys(node.attributes ?? {}).length > 0)
  );
}

/**
 * omits attributes reconstructed by the registered parser from the emitted body
 * @param node original semantic block
 * @param body custom representation
 * @param context ordered adapter registry and inclusion controls
 * @returns an immutable envelope projection
 */
export function omitInferredAttributes(
  node: MdcxFlowContent<string>,
  body: string,
  context: WriterContext,
): MdcxFlowContent<string> {
  const attributes = Object.entries(node.attributes ?? {});
  if (!context.includes.includes('attributes') || attributes.length === 0) {
    return node;
  }
  let inferredChildren: MdcxFlowContent<string>[];
  try {
    inferredChildren = parse(body, { adapters: context.adapters }).ast
      .children;
  } catch {
    return node;
  }
  const inferred = inferredChildren[0];
  if (
    !inferred ||
    inferredChildren.length !== 1 ||
    inferred.type !== node.type ||
    (inferred.kind ?? 'common') !== (node.kind ?? 'common')
  ) {
    return node;
  }

  const inferredAttributes = inferred.attributes ?? {};

  return {
    ...node,
    attributes: Object.fromEntries(
      attributes.filter(
        ([key, value]) =>
          !Object.hasOwn(inferredAttributes, key) ||
          !isDeepStrictEqual(value, inferredAttributes[key]),
      ),
    ),
  };
}
