import { ParseError } from '#errors';

import { decodeString } from '#syntax/inline/literals';

import {
  getChildren,
  isNode,
  isRoot,
  resolveNodeKind,
  validateNodeKind,
  validateTree,
} from '#tree';

import { applyAdapters } from './adapters';
import { parseInline } from './inline';

import type { BlockEvent, SourceContext } from '#lexer/types';

import type { Attributes, MdcxNode, MdcxRoot } from '#types';

/**
 * builds the single semantic tree from resolved syntax events
 * @param events resolved document block syntax
 * @param context shared parsing and source context
 * @param metadata document semantic selectors and attributes
 * @returns the resolved semantic document
 */
export function buildAst(
  events: readonly BlockEvent[],
  context: SourceContext,
  metadata: Attributes = {},
): MdcxRoot<string> {
  const defaults = context.options.document ?? {};
  validateDocumentSelectors([defaults, metadata]);
  const kind = metadata.kind ?? defaults.kind ?? 'common';
  validateNodeKind(kind);
  const type = metadata.type ?? defaults.type ?? 'root';
  const ref = metadata.ref ?? defaults.ref;
  const attributes = {
    ...defaults.attributes,
    ...Object.fromEntries(
      Object.entries(metadata).filter(
        ([key]) => !['kind', 'type', 'ref'].includes(key),
      ),
    ),
  };
  const eventNodes = new Map<object, BlockEvent>();
  const root = {
    type,
    kind: kind === 'common' ? undefined : kind,
    form: kind === 'common' ? undefined : 'document',
    ref,
    attributes,
    children: events.map((event) => buildEvent(event, context, eventNodes)),
    range: context.locate(0, context.source.length),
  };
  validateTree(root);

  const adapted = applyAdapters(root, context, eventNodes);
  validateTree(adapted);

  if (!context.options.onNode) {
    return adapted;
  }

  const transformed = transformNode(adapted, context);

  if (transformed === null || !isRoot(transformed)) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'onNode must retain one document root.',
    );
  }

  validateTree(transformed);

  return transformed;
}

/**
 * applies explicit semantic transformations bottom-up exactly once
 * @param node canonical semantic node
 * @param context shared parsing and source context
 * @returns the resolved transformed node or null
 */
export function transformNode(
  node: MdcxNode<string>,
  context: SourceContext,
): MdcxNode<string> | null {
  const updated =
    'children' in node
      ? {
          ...node,
          children: getChildren(node)
            .map((child) => transformNode(child, context))
            .filter((child) => child !== null),
        }
      : node;

  if (!isNode(updated)) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'onNode produced invalid child containment.',
    );
  }

  const result = context.options.onNode?.(updated);

  if (result === undefined) {
    return updated;
  }

  if (result !== null && !isNode(result)) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'onNode returned an invalid semantic node.',
    );
  }

  return result;
}

/**
 * validates document classification before applying per-field precedence
 * @param entries configured and source metadata mappings
 */
function validateDocumentSelectors(entries: readonly object[]): void {
  for (const selectors of entries) {
    for (const key of ['kind', 'type', 'ref']) {
      if (!Object.prototype.hasOwnProperty.call(selectors, key)) {
        continue;
      }

      const value = Object.entries(selectors).find(
        ([name]) => name === key,
      )?.[1];

      if (
        typeof value !== 'string' ||
        !(
          key === 'ref' ? /^\w[A-Za-z0-9_-]*$/ : /^[A-Za-z][A-Za-z0-9_-]*$/
        ).test(value)
      ) {
        throw new ParseError(
          'MDC_AST_INVALID',
          `Document ${key} must be a valid nonempty selector.`,
        );
      }
    }
  }
}

/**
 * constructs fields already resolved by the lexical front end
 * @param event resolved block syntax
 * @param context shared parsing and source context
 * @param eventNodes semantic ownership of lexical bodies
 * @returns the resolved constructed semantic fields
 */
function buildEvent(
  event: BlockEvent,
  context: SourceContext,
  eventNodes: Map<object, BlockEvent>,
): Record<string, unknown> {
  context.budget.enter();

  try {
    // block HTML has its own default namespace in MDCX; explicit selectors,
    // the resolver and configured default still win, and CommonMark is unchanged
    const kind =
      context.options.profile === 'commonmark'
        ? 'common'
        : resolveNodeKind(
            {
              type: event.type,
              form: 'block',
              attributes: event.attributes ?? {},
              ref: event.ref,
            },
            context.options,
            {
              explicitKind: event.namespace,
              fallback: event.isCustom
                ? 'custom'
                : event.type === 'html'
                  ? 'html'
                  : 'common',
            },
          );
    const node: Record<string, unknown> = {
      type: event.type,
      kind: kind === 'common' ? undefined : kind,
      form: kind === 'common' ? undefined : 'block',
      ref: event.ref,
      attributes: event.attributes,
      range: context.locate(
        event.view.mapOffset(0),
        event.view.mapOffset(event.view.text.length),
      ),
    };

    if (event.children) {
      node.children = event.children.map((child) =>
        buildEvent(child, context, eventNodes),
      );
    }

    if (event.inline) {
      node.children = parseInline(event.inline, context);
    }

    if (event.isCustom) {
      node.children ??= [];
    } else {
      Object.assign(node, getNativeFields(event));
    }
    eventNodes.set(node, event);

    return node;
  } finally {
    context.budget.leave();
  }
}

/**
 * maps resolved native event fields without rediscovering source syntax
 * @param event resolved native block
 * @returns native semantic fields
 */
function getNativeFields(event: BlockEvent): Record<string, unknown> {
  const node: Record<string, unknown> = {};

  switch (event.type) {
    case 'heading':
      node.depth = event.depth;
      break;
    case 'list':
      node.ordered = event.ordered;
      node.start = event.ordered ? event.start : null;
      node.spread = event.spread;
      break;
    case 'listItem':
      node.spread = event.spread;

      if (event.checked !== null && event.checked !== undefined) {
        node.checked = event.checked;
      }

      break;
    case 'code':
      node.value = event.value;
      node.lang = event.lang ? decodeString(event.lang) : null;
      node.meta = event.meta ? decodeString(event.meta) : null;
      break;
    case 'html':
    case 'math':
      node.value = event.value;
      break;
    case 'definition':
      Object.assign(node, event.definition);
      break;
    case 'table':
      node.align = event.align;
      break;
    default:
      break;
  }

  return node;
}
