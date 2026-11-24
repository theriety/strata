import { readHtml } from '#syntax/inline/html';

import { decodeString, normalizeIdentifier } from '#syntax/inline/literals';
import { projectAlternativeText } from '#syntax/inline/projection';

import { isMarkdownShape, validateTree } from '#tree';

import { formatInlineAnnotation } from './annotation';

import { createWriterContext } from './context';
import { findDelimiter } from './delimiter';
import { StringifyError } from './errors';
import { selectFormattingMarker, writePlainText } from './inline/formatting';
import {
  hasMatchingFormattingContent,
  hasMatchingReferenceContent,
} from './inline/reference';
import { hasMetadata, hasSelectedMetadata } from './metadata';
import {
  formatAssociationLabel,
  formatDestination,
  formatResourceTitle,
} from './resource';
import { escapeText } from './text';

import type { MarkdownShape } from '#tree/validation';

import type { MdcxPhrasingContent } from '#types';

import type { StringifyOptions, WriterContext } from './types';

const FORMATTING_CANDIDATE_COUNT = 4;

const FLAT_FORMATTING_CANDIDATE = 3;

/**
 * serializes recursive phrasing through the public boundary
 * @param nodes semantic inline nodes
 * @param options canonical output configuration
 * @returns canonical inline source
 */
export function stringifyInlineContent<A extends string>(
  nodes: ReadonlyArray<MdcxPhrasingContent<A>>,
  options: StringifyOptions = {},
): string {
  validateTree({
    type: 'root',
    children: [{ type: 'paragraph', children: [...nodes] }],
  });

  return writeInline(nodes, createWriterContext(options));
}

/**
 * renders validated phrasing without flattening formatting or rich labels
 * @param nodes ordered phrasing children
 * @param context output configuration
 * @returns inline Markdown and MDCX source
 */
export function writeInline(
  nodes: ReadonlyArray<MdcxPhrasingContent<string>>,
  context: WriterContext,
): string {
  if (
    (context.profile !== 'commonmark' && hasAnnotatedContent(nodes, context)) ||
    context.formattingCandidate !== undefined
  ) {
    return writeInlineSequence(nodes, context);
  }

  for (let candidate = 0; candidate < FORMATTING_CANDIDATE_COUNT; candidate++) {
    const source = writeInlineSequence(nodes, {
      ...context,
      formattingCandidate: candidate,
      formattingMarker: candidate % 2 ? '_' : '*',
    });

    if (hasMatchingFormattingContent(source, nodes, context)) {
      return source;
    }
  }

  // Nested emphasis and strong nodes have one canonical, unambiguous Markdown
  // spelling. The delimiter validator can reject equivalent marker choices
  // when application metadata is present elsewhere in the phrasing run.
  if (nodes.some(hasNestedEmphasis)) {
    return writeInlineSequence(nodes, {
      ...context,
      formattingCandidate: FLAT_FORMATTING_CANDIDATE,
      formattingMarker: '*',
    });
  }

  throw new StringifyError(
    'Native inline delimiter arrangement cannot preserve this phrasing tree.',
  );
}

function hasNestedEmphasis(
  node: MdcxPhrasingContent<string>,
): boolean {
  return (
    (node.type === 'strong' || node.type === 'emphasis') &&
    'children' in node &&
    node.children.length === 1 &&
    (node.children[0]?.type === 'strong' || node.children[0]?.type === 'emphasis')
  );
}

/**
 * constructs one bounded delimiter candidate with distinct adjacent marker runs
 * @param nodes ordered phrasing nodes
 * @param context candidate marker selection and output configuration
 * @returns candidate inline source
 */
function writeInlineSequence(
  nodes: ReadonlyArray<MdcxPhrasingContent<string>>,
  context: WriterContext,
): string {
  const parts: string[] = [];
  let text = '';
  let previousMarker: '*' | '_' | undefined;

  for (const node of nodes) {
    if (
      node.type === 'text' &&
      'value' in node &&
      (!hasMetadata(node) ||
        (!hasSelectedMetadata(node, context) &&
          context.profile !== 'commonmark'))
    ) {
      text += node.value;
      previousMarker = undefined;
      continue;
    }

    if (text !== '') {
      parts.push(
        context.isInReferenceLabel
          ? formatReferenceText(text)
          : writePlainText(text, context),
      );
      text = '';
    }

    previousMarker = selectFormattingMarker(node, context, previousMarker);
    parts.push(
      writeInlineNode(node, { ...context, formattingMarker: previousMarker }),
    );
  }

  if (text !== '') {
    parts.push(
      context.isInReferenceLabel
        ? formatReferenceText(text)
        : writePlainText(text, context),
    );
  }

  return parts.join('');
}

/**
 * selects native semantics only after complete shape validation
 * @param node semantic phrasing node
 * @param context output configuration
 * @returns one inline construct
 */
function writeInlineNode(
  node: MdcxPhrasingContent<string>,
  context: WriterContext,
): string {
  const isEmptyApplication =
    (node.kind ?? 'common') !== 'common' &&
    ['emphasis', 'strong', 'delete'].includes(node.type) &&
    'children' in node &&
    node.children.length === 0;

  if (!isMarkdownShape(node) || isEmptyApplication) {
    if (context.profile === 'commonmark') {
      throw new StringifyError(
        'CommonMark cannot represent application inline constructs.',
      );
    }

    const body = writeInline(node.children, context);

    return formatInlineAnnotation({ node, body, context, isNamed: true });
  }

  if (
    context.profile === 'commonmark' &&
    (hasMetadata(node) || ['inlineMath', 'delete'].includes(node.type))
  ) {
    throw new StringifyError(
      `CommonMark cannot represent ${node.type} extensions or semantic metadata.`,
    );
  }

  const body = writeMarkdownInline(node, context);

  return node.type === 'html'
    ? body
    : formatInlineAnnotation({ node, body, context, isNamed: false });
}

/**
 * serializes standard phrasing fields with context-safe delimiters
 * @param node validated Markdown shape
 * @param context output configuration
 * @returns native inline source
 */
function writeMarkdownInline(
  node: Extract<MarkdownShape, MdcxPhrasingContent<string>>,
  context: WriterContext,
): string {
  switch (node.type) {
    case 'text':
      if (node.value === '' && hasSelectedMetadata(node, context)) {
        throw new StringifyError(
          'Empty annotated native text has no inferred Markdown target.',
        );
      }

      return escapeText(node.value);
    case 'inlineCode':
      return writeCodeSpan(
        context.isInTable ? node.value.replace(/\|/g, '\\|') : node.value,
      );
    case 'inlineMath':
      throw new StringifyError(
        'Inline math has no approved MDCX source grammar; only whole-line display math is supported.',
      );
    case 'html': {
      if (hasMetadata(node) && hasSelectedMetadata(node, context)) {
        throw new StringifyError(
          'HTML annotations require a block target; inline HTML cannot preserve semantic metadata.',
        );
      }

      if (readHtml(node.value) !== node.value) {
        throw new StringifyError(
          'Inline HTML must contain exactly one representable HTML token.',
        );
      }

      return node.value;
    }
    case 'break':
      return '\\\n';
    case 'emphasis':
    case 'strong':
    case 'delete':
      return writeFormatting(node, context);
    case 'link':
      return `[${writeLinkContent(node, context)}](${formatDestination(node.url)}${formatResourceTitle(node.title)})`;
    case 'image':
      return `![${escapeText(node.alt ?? '')}](${formatDestination(node.url)}${formatResourceTitle(node.title)})`;
    case 'linkReference':
      return writeReference(node, writeLinkContent(node, context), context);
    default:
      return `!${writeReference(node, escapeText(node.alt ?? ''), context)}`;
  }
}

/**
 * verifies label formatting independently across the resource bracket boundary
 * @param node native link or reference link
 * @param context containing output configuration
 * @returns recursively serialized label
 */
function writeLinkContent(
  node: Extract<MarkdownShape, { type: 'link' | 'linkReference' }>,
  context: WriterContext,
): string {
  return writeInline(node.children, {
    ...context,
    formattingCandidate: undefined,
    isInReferenceLabel:
      context.isInReferenceLabel ??
      (node.type === 'linkReference' && node.referenceType !== 'full'),
  });
}

/**
 * preserves formatting nesting with native delimiters
 * @param node semantic formatting node
 * @param context active output context
 * @returns delimited phrasing
 */
function writeFormatting(
  node: Extract<MarkdownShape, { type: 'emphasis' | 'strong' | 'delete' }>,
  context: WriterContext,
): string {
  const nested = node.children.length === 1 ? node.children[0] : undefined;
  if (
    (node.type === 'strong' || node.type === 'emphasis') &&
    nested &&
    (nested.type === 'strong' || nested.type === 'emphasis') &&
    nested.type !== node.type &&
    context.profile !== 'commonmark'
  ) {
    return `***${writeInline(nested.children, context)}***`;
  }
  const selected = context.formattingMarker ?? '*';
  const body = writeInline(node.children, {
    ...context,
    formattingMarker:
      (context.profile === 'commonmark' ||
        !context.includes.includes('type')) &&
      context.formattingCandidate !== FLAT_FORMATTING_CANDIDATE
        ? selected === '*'
          ? '_'
          : '*'
        : selected,
  });

  if (body === '') {
    throw new StringifyError(
      `Empty ${node.type} has no native Markdown representation.`,
    );
  }

  const marker =
    node.type === 'emphasis'
      ? selected
      : node.type === 'strong'
        ? selected.repeat(2)
        : '~~';

  return `${marker}${body}${marker}`;
}

/**
 * chooses a code-span fence and padding that preserves literal spaces
 * @param value opaque inline-code literal
 * @returns code-span source
 */
function writeCodeSpan(value: string): string {
  if (value.includes('\0')) {
    throw new StringifyError('Inline code cannot preserve NUL.');
  }

  const fence = findDelimiter(value, '`', 1);
  const shouldPad =
    value.startsWith('`') ||
    value.endsWith('`') ||
    (value.startsWith(' ') && value.endsWith(' ') && /[^ ]/.test(value));

  return `${fence}${shouldPad ? ' ' : ''}${value}${shouldPad ? ' ' : ''}${fence}`;
}

/**
 * preserves full, collapsed, and shortcut reference association forms
 * @param node reference-bearing Markdown node
 * @param label emitted label content
 * @param context document reference membership
 * @returns reference source
 */
function writeReference(
  node: Extract<MarkdownShape, { type: 'linkReference' | 'imageReference' }>,
  label: string,
  context: WriterContext,
): string {
  if (!context.definitions.has(node.identifier)) {
    throw new StringifyError(
      `Reference ${node.identifier} requires a definition in the serialized document.`,
    );
  }

  const association = formatAssociationLabel(
    node.identifier,
    node.label ?? undefined,
  );
  let content = label;

  if (node.referenceType !== 'full') {
    if (
      node.type === 'imageReference'
        ? projectAlternativeText({
            source: association,
            definitions: context.definitions,
          }) === (node.alt ?? '')
        : hasMatchingReferenceContent(association, node.children, context)
    ) {
      content = association;
    }

    if (
      normalizeIdentifier(content) !== node.identifier ||
      decodeString(content) !== (node.label ?? decodeString(node.identifier))
    ) {
      throw new StringifyError(
        'Collapsed and shortcut reference content must preserve its raw identifier and decoded label.',
      );
    }
  }

  const suffix =
    node.referenceType === 'full'
      ? `[${association}]`
      : node.referenceType === 'collapsed'
        ? '[]'
        : '';

  return `[${content}]${suffix}`;
}

/**
 * escapes active label punctuation while retaining source association whitespace
 * @param value semantic text
 * @returns literal label source
 */
function formatReferenceText(value: string): string {
  return value.replace(/[\\`*_[\]<>&]/g, '\\$&');
}

/**
 * detects annotations anywhere inside a native formatting candidate
 * @param nodes complete phrasing subtree
 * @param context selected metadata categories
 * @returns whether native-only delimiter projection would discard annotations
 */
function hasAnnotatedContent(
  nodes: ReadonlyArray<MdcxPhrasingContent<string>>,
  context: WriterContext,
): boolean {
  return nodes.some(
    (node) =>
      hasSelectedMetadata(node, context) ||
      ('children' in node && hasAnnotatedContent(node.children, context)),
  );
}
