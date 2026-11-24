import { ParseError } from '#errors';

import { parseAnnotationMapping } from '#parser/annotation';

import { readResource } from '#syntax/inline/literals';

import { skipInlineOpaque } from './boundary';
import { findFlowMappingEnd } from './mapping';

import { trimSpace, trimSpaceEnd } from './whitespace';

import type { Attributes, SourcePosition } from '#types';

import type { BlockEvent, SourceContext, SourceView } from './types';

/** syntactic opening selector, independent of semantic namespace resolution */
export interface BlockHeader {
  content: number;
  indent: number;
  namespace?: string;
  type?: string;
  attributes?: Attributes;
  ref?: string;
}

/** shared coordinates for one closing assertion */
export interface ClosingSource {
  context: SourceContext;
  view: SourceView;
}

export const STRUCTURAL_TYPES: Readonly<Record<string, string>> = {
  item: 'listItem',
  row: 'tableRow',
  cell: 'tableCell',
};

/**
 * recognizes complete header controls with horizontal spacing only
 * @param view current line with original boundary mapping
 * @param context shared parsing and source coordinates
 * @returns the resolved header or null for ordinary text
 */
export function readHeader(
  view: SourceView,
  context: SourceContext,
): BlockHeader | null {
  const { text } = view;
  const prefix = /^ {0,3}::[ \t]*/.exec(text);

  if (!prefix || text[prefix[0].length] === ':') {
    return null;
  }

  let cursor = prefix[0].length;
  const selector =
    /^([A-Za-z][A-Za-z0-9_-]*)(?::([A-Za-z][A-Za-z0-9_-]*)?)?/.exec(
      text.slice(cursor),
    );
  const header: BlockHeader = {
    content: cursor,
    indent: text.indexOf('::'),
  };

  if (selector) {
    cursor += selector[0].length;

    if (selector[0].includes(':')) {
      header.namespace = selector[1];
      header.type = selector[2];
    } else {
      header.type = selector[1];
    }

    if (text.charAt(cursor) && !/[ \t{#]/.test(text.charAt(cursor))) {
      return null;
    }
  }

  const suffix = readHeaderSuffix(view, cursor, context);

  if (!selector && !suffix.attributes && !suffix.ref) {
    return null;
  }

  return { ...header, ...suffix };
}

/**
 * recognizes an outer one-line terminator while protecting inline opaque contexts
 * @param text current line text
 * @param start inclusive region boundary
 * @returns the resolved closing marker index, or -1
 */
export function findClosingIndex(text: string, start: number): number {
  let brackets = 0;
  let state = { index: start, ticks: 0 };

  while (state.index < text.length) {
    const opaque = skipInlineOpaque(text, state.index, state.ticks);

    if (opaque) {
      state = opaque;
      continue;
    }

    const index = state.index;
    const resource =
      text[index] === '(' && text[index - 1] === ']'
        ? readResource(text, index)
        : undefined;

    if (resource) {
      state.index = resource.end;
      continue;
    }

    if (text[index] === '[') {
      brackets++;
    }

    if (text[index] === ']') {
      brackets = Math.max(0, brackets - 1);
    }

    if (
      !brackets &&
      text.slice(index, index + 2) === ';;' &&
      /^(?:#\w[A-Za-z0-9_-]*)?$/.test(trimSpace(text.slice(index + 2)))
    ) {
      return index;
    }

    state.index++;
  }

  return -1;
}

/**
 * verifies local closing assertion without changing ownership
 * @param text current line text
 * @param header recognized opening controls
 * @param source shared parsing context and mapped source region
 */
export function assertCloser(
  text: string,
  header: BlockHeader,
  source: ClosingSource,
): void {
  const { context, view } = source;
  const close = /^;;(?:[ \t]*#(\w[A-Za-z0-9_-]*))?$/.exec(trimSpaceEnd(text));

  if (!close) {
    throw new ParseError(
      'MDC_CLOSING_MARKER_INVALID',
      'Malformed closing marker.',
      context.locate(view.mapOffset(0), view.mapOffset(view.text.length)).start,
    );
  }

  if (close[1] !== undefined && close[1] !== header.ref) {
    const message = `Closing reference ${close[1]} does not match opening reference ${header.ref ?? '(absent)'}.`;
    const range = context.locate(
      view.mapOffset(0),
      view.mapOffset(view.text.length),
    );

    if (context.options.mode === 'recover') {
      context.options.onDiagnostic({
        code: 'MDC_CLOSING_MARKER_MISMATCH',
        severity: 'warning',
        message: `${message} Ignored the closing assertion; retained opening identity and indentation ownership.`,
        range,
      });

      return;
    }

    throw new ParseError('MDC_CLOSING_MARKER_MISMATCH', message, range.start);
  }
}

/**
 * attaches syntactic metadata directly to the complete inferred target
 * @param event resolved block syntax
 * @param header recognized opening controls
 * @param context shared source coordinates
 * @returns the resolved annotated target
 * @throws {ParseError} when annotations contain conflicting declarations
 */
export function annotateEvent<Event extends BlockEvent>(
  event: Event,
  header: BlockHeader,
  context: SourceContext,
): Omit<Event, 'namespace' | 'attributes' | 'ref'> &
  Pick<BlockEvent, 'namespace' | 'attributes' | 'ref'> {
  const position = context.locate(
    event.view.mapOffset(0),
    event.view.mapOffset(event.view.text.length),
  ).start;

  // direct annotations share one target, so no outer envelope may erase a
  // declaration before identity validation and node transformations can see it
  if (event.ref !== undefined && header.ref !== undefined && event.ref !== header.ref) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'A block target cannot declare two references.',
      position,
    );
  }

  if (
    event.namespace !== undefined &&
    header.namespace !== undefined &&
    event.namespace !== header.namespace
  ) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'A block annotation cannot overwrite its target namespace.',
      position,
    );
  }

  const outerAttributes =
    Object.keys(header.attributes ?? {}).length > 0
      ? header.attributes
      : undefined;

  if (outerAttributes && Object.keys(event.attributes ?? {}).length > 0) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'A block annotation cannot overwrite attributes already declared on its target.',
      position,
    );
  }

  assertTargetType(event.type, header, position);

  return {
    ...event,
    namespace: header.namespace ?? event.namespace,
    attributes: outerAttributes ?? event.attributes,
    ref: header.ref ?? event.ref,
  };
}

/**
 * reads horizontal spacing, a balanced mapping and the opening reference
 * @param view current header line and original boundaries
 * @param start first suffix offset
 * @param context shared original source coordinates
 * @returns parsed suffix and body boundary
 */
function readHeaderSuffix(
  view: SourceView,
  start: number,
  context: SourceContext,
): Pick<BlockHeader, 'attributes' | 'ref' | 'content'> {
  const { text } = view;
  let cursor = start;
  let attributes: Attributes | undefined;
  let ref: string | undefined;

  while (text[cursor] === ' ' || text[cursor] === '\t') {
    cursor++;
  }

  if (text[cursor] === '{') {
    const mappingStart = cursor;
    const getPosition = (offset: number): SourcePosition =>
      context.locate(
        view.mapOffset(mappingStart + offset),
        view.mapOffset(mappingStart + offset),
      ).start;
    const end = findFlowMappingEnd({
      source: text,
      startIndex: cursor,
      options: { position: getPosition(0) },
    });

    if (end === null) {
      throw new ParseError(
        'MDC_ANNOTATION_INVALID',
        'Unclosed block attribute mapping.',
        getPosition(0),
      );
    }

    attributes = parseAnnotationMapping({
      mapping: end,
      context: { name: 'Block annotation', position: getPosition(0) },
      getPosition,
    });
    cursor = end.end;
  }

  while (text[cursor] === ' ' || text[cursor] === '\t') {
    cursor++;
  }

  if (text[cursor] === '#') {
    const reference = /^#(\w[A-Za-z0-9_-]*)/.exec(text.slice(cursor));

    if (reference) {
      ref = reference[1];
      cursor += reference[0].length;
    }
  }

  while (text[cursor] === ' ' || text[cursor] === '\t') {
    cursor++;
  }

  return { attributes, ref, content: cursor };
}

/**
 * verifies an explicit native classification without changing the target
 * @param type recognized target classification
 * @param header opening controls
 * @param position original target start
 * @throws {ParseError} when the explicit native classification mismatches its target
 */
function assertTargetType(
  type: string,
  header: BlockHeader,
  position: SourcePosition,
): void {
  const expected =
    header.type && (STRUCTURAL_TYPES[header.type] ?? header.type);

  if (expected && header.namespace === 'common' && expected !== type) {
    throw new ParseError(
      'MDC_AST_INVALID',
      `Expected ${expected} target, found ${type}.`,
      position,
    );
  }
}
