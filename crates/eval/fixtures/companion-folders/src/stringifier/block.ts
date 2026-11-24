import { isMarkdownFlowShape } from '#tree';

import { formatBlockAnnotation, formatLineAnnotation } from './annotation';
import {
  writeCodeBlock,
  writeMathBlock,
  writeHtmlBlock,
} from './block/literal';
import { StringifyError } from './errors';

import { indent } from './indentation';
import { writeInline } from './inline';
import { hasMetadata, omitInferredAttributes } from './metadata';
import {
  formatAssociationLabel,
  formatDestination,
  formatResourceTitle,
} from './resource';
import { stringifyTable } from './table';

import type { MarkdownShape } from '#tree/validation';

import type { MarkdownFlowShape } from '#tree/validation/content';

import type { MdcxFlowContent, MdcxListItem } from '#types';

import type { WriterContext } from './types';

type MarkdownList = Extract<MarkdownShape, { type: 'list' }>;

const MAX_ORDERED_MARKER = 999999999;

/**
 * separates complete blocks without losing container boundaries
 * @param nodes ordered flow children
 * @param context output configuration
 * @param separator spacing between already complete block constructs
 * @returns canonical block source
 */
export function writeFlow(
  nodes: ReadonlyArray<MdcxFlowContent<string>>,
  context: WriterContext,
  separator = '\n\n',
): string {
  let listIndex = 0;

  return nodes
    .map((node, index) => {
      if (
        context.profile === 'commonmark' &&
        separator === '\n' &&
        nodes[index - 1]?.type === 'paragraph' &&
        ['paragraph', 'definition'].includes(node.type)
      ) {
        throw new StringifyError(
          'Tight CommonMark list items cannot preserve a paragraph followed by a paragraph or definition without a blank line.',
        );
      }

      const previous = nodes[index - 1];
      listIndex =
        node.type === 'list' && previous?.type === 'list' ? listIndex + 1 : 0;

      return stringifyBlock(node, context, listIndex);
    })
    .filter((body) => body !== '')
    .join(separator);
}

/**
 * dispatches on validated native shapes and explicit application form
 * @param node semantic flow node
 * @param context output configuration
 * @param index sibling index used to separate adjacent native lists
 * @returns canonical complete block
 */
export function stringifyBlock(
  node: MdcxFlowContent<string>,
  context: WriterContext,
  index = 0,
): string {
  const customBody = writeAdapterBody(node, context, index);
  if (customBody !== null) {
    return formatBlockAnnotation({
      node: omitInferredAttributes(node, customBody, context),
      body: customBody,
      context,
      isNamed: true,
    });
  }

  const isEmptyApplicationParagraph =
    (node.kind ?? 'common') !== 'common' &&
    node.type === 'paragraph' &&
    'children' in node &&
    node.children.length === 0;

  if (isEmptyApplicationParagraph) {
    if (context.profile === 'commonmark') {
      throw new StringifyError(
        'CommonMark cannot represent application blocks.',
      );
    }

    return formatBlockAnnotation({ node, body: '', context, isNamed: true });
  }

  if (!isMarkdownFlowShape(node)) {
    if (context.profile === 'commonmark') {
      throw new StringifyError(
        'CommonMark cannot represent application blocks.',
      );
    }

    return formatBlockAnnotation({
      node,
      body: writeFlow(node.children, context),
      context,
      isNamed: true,
    });
  }

  if (context.profile === 'commonmark' && hasMetadata(node)) {
    throw new StringifyError(
      'CommonMark cannot represent node namespace, attributes, or reference metadata.',
    );
  }

  const body = writeMarkdownBlock(node, context, index);
  const annotationNode = omitInferredNativeAttributes(node);

  if (context.profile === 'commonmark') {
    return body;
  }

  if (
    ['paragraph', 'heading', 'thematicBreak', 'definition'].includes(
      node.type,
    ) &&
    !body.includes('\n')
  ) {
    return formatLineAnnotation(annotationNode, body, context);
  }

  return formatBlockAnnotation({ node: annotationNode, body, context, isNamed: false });
}

/**
 * emits native block fields without retagging or flattening children
 * @param node validated Markdown shape
 * @param context output configuration
 * @param index sibling index for collision-free adjacent lists
 * @returns native source
 */
function writeMarkdownBlock(
  node: MarkdownFlowShape,
  context: WriterContext,
  index: number,
): string {
  switch (node.type) {
    case 'paragraph': {
      const body = writeInline(node.children, context);

      if (
        body === '' &&
        (context.includes.includes('type') ||
          context.profile === 'commonmark' ||
          node.children.length === 0)
      ) {
        throw new StringifyError(
          'Empty native paragraphs have no Markdown source representation.',
        );
      }

      return body;
    }
    case 'heading':
      return `${'#'.repeat(node.depth)} ${writeInline(node.children, context)}`;
    case 'thematicBreak':
      return '***';
    case 'blockquote': {
      if (node.children.length === 0) {
        return '>';
      }

      return indent(writeFlow(node.children, context), '> ');
    }
    case 'list':
      return writeList(node, context, index);
    case 'code':
      return writeCodeBlock(node);
    case 'math':
      return writeMathBlock(node, context);
    case 'html':
      return writeHtmlBlock(node.value);
    case 'definition':
      return `[${formatAssociationLabel(node.identifier, node.label ?? undefined)}]: ${formatDestination(node.url)}${formatResourceTitle(node.title)}`;
    default:
      return stringifyTable(node, context);
  }
}


/** Removes table metadata that is reconstructed from native table shape. */
function omitInferredNativeAttributes<T extends MdcxFlowContent<string>>(node: T): T {
  if (node.type !== 'table' || !node.attributes) return node;
  const attributes = { ...node.attributes };
  delete attributes.table_width;
  delete attributes.has_column_header;
  return Object.keys(attributes).length ? { ...node, attributes } : { ...node, attributes: undefined };
}
/**
 * preserves list grouping, starting ordinal, task state and item blocks
 * @param node validated native list
 * @param context output configuration
 * @param index sibling index for native marker alternation
 * @returns complete list source
 */
function writeList(
  node: MarkdownList,
  context: WriterContext,
  index: number,
): string {
  if (node.children.length === 0) {
    throw new StringifyError(
      'An empty list has no native Markdown representation.',
    );
  }

  if (
    (node.spread && node.children.length === 1 && !node.children[0]?.spread) ||
    (!node.spread && node.children.some((item) => item.spread))
  ) {
    throw new StringifyError(
      'List spread conflicts with its item spacing and cannot be represented independently.',
    );
  }

  const separator = node.spread ? '\n\n' : '\n';

  return node.children
    .map((item, itemIndex) => {
      const marker = node.ordered
        ? `${Math.min((node.start ?? 1) + itemIndex, MAX_ORDERED_MARKER)}${context.profile === 'commonmark' && index % 2 ? ')' : '.'}`
        : index % 2
          ? '+'
          : '-';

      return writeListItem(item, marker, context);
    })
    .join(separator);
}

/**
 * emits each list item as one marker and correctly indented child blocks
 * @param item semantic list item
 * @param marker native list marker
 * @param context output configuration
 * @returns item source
 */
function writeListItem(
  item: MdcxListItem<string>,
  marker: string,
  context: WriterContext,
): string {
  if (
    context.profile === 'commonmark' &&
    (hasMetadata(item) || typeof item.checked === 'boolean')
  ) {
    throw new StringifyError(
      'CommonMark cannot represent task state or list-item metadata.',
    );
  }

  const separator = item.spread ? '\n\n' : '\n';
  const body = writeFlow(item.children, context, separator);
  const task =
    typeof item.checked !== 'boolean' ? '' : item.checked ? '[x] ' : '[ ] ';
  const lines = body.split('\n');
  const prefix = ' '.repeat(marker.length + 1);
  const source = lines
    .map((line, index) =>
      index === 0 ? `${marker} ${task}${line}` : `${prefix}${line}`,
    )
    .join('\n');

  return context.profile === 'commonmark'
    ? source
    : formatBlockAnnotation({
        node:
          item.attributes?.ordered === /^\d/.test(marker)
            ? {
                ...item,
                attributes: Object.fromEntries(
                  Object.entries(item.attributes).filter(
                    ([key]) => key !== 'ordered',
                  ),
                ),
              }
            : item,
        body: source,
        context,
        isNamed: false,
      });
}

/**
 * writes the current body natively while descendants retain adapter dispatch
 * @param node semantic block
 * @param context inherited output controls
 * @param index native sibling position
 * @returns body source without the current annotation envelope
 */
function stringifyBody(
  node: MdcxFlowContent<string>,
  context: WriterContext,
  index: number,
): string {
  return isMarkdownFlowShape(node)
    ? writeMarkdownBlock(node, context, index)
    : writeFlow(node.children, context);
}

/**
 * dispatches a body codec without moving package-owned envelopes into callers
 * @param node validated native block
 * @param context inherited output configuration
 * @param index sibling position for native default writing
 * @returns custom body or native fallback
 */
function writeAdapterBody(
  node: MdcxFlowContent<string>,
  context: WriterContext,
  index: number,
): string | null {
  const adapter = context.adapters?.[node.type];
  if (
    context.profile !== 'commonmark' &&
    adapter?.custom &&
    (adapter.kind === undefined || adapter.kind === (node.kind ?? 'common'))
  ) {
    let body: string | null;
    try {
      body = adapter.custom.stringify(node, {
        stringifyBlock: (child) => stringifyBlock(child, context),
        stringifyBlocks: (children) => writeFlow(children, context),
        stringifyInline: (children) => writeInline(children, context),
        defaultStringify: (target) => stringifyBody(target, context, index),
      });
    } catch (cause) {
      const error = new StringifyError(
        `Adapter stringifier failed for ${node.type}.`,
      );
      error.cause = cause;
      throw error;
    }
    if (body !== null) {
      if (typeof body !== 'string') {
        throw new StringifyError('Adapter must return a string or null.');
      }

      return body;
    }
  }

  return null;
}
