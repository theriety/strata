import { ParseError } from '#errors';

import { isListEvent, recognizeBlocks } from '../block';
import {
  annotateEvent,
  assertCloser,
  findClosingIndex,
  readHeader,
  STRUCTURAL_TYPES,
} from '../controls';
import { getExtent, sliceView, joinViews } from '../source';

import { trimSpaceEnd, trimSpace } from '../whitespace';

import { readOwnedBody } from './mdcx-body';
import { readHtmlStart } from './opaque';

import type { BlockResult } from '../block';
import type { BlockHeader } from '../controls';

import type { BlockEvent, BlockSource, SourceLine } from '../types';

import type { BlockInput } from './mdcx-body';

export {
  annotateEvent,
  assertCloser,
  findClosingIndex,
  readHeader,
} from '../controls';

/**
 * recognizes a whole block envelope with indentation-owned multiline bodies
 * @param source existing current line and its shared source context
 * @returns the resolved block and next line, or null
 */
export function readMdcxBlock(source: BlockSource): BlockResult | null {
  const { lines, index, context, line } = source;
  const header = readHeader(line.view, context);

  if (!header) {
    return null;
  }

  const rest = line.view.text.slice(header.content);
  let following = index + 1;

  while (following < lines.length && !trimSpace(lines[following]?.view.text)) {
    following++;
  }

  const nextText = lines[following]?.view.text ?? '';
  const inferredStructural =
    header.type &&
    STRUCTURAL_TYPES[header.type] &&
    (rest.startsWith('|') ||
      (!rest && /^(?:[-+*][ \t]|\d{1,9}[.)][ \t]|\|)/.test(nextText)));
  // a qualified html selector annotates a complete literal target directly;
  // a named html construct whose body is Markdown retains its own children
  const isNamed =
    header.type !== undefined &&
    header.namespace !== 'common' &&
    !inferredStructural &&
    !(
      header.type === 'html' &&
      readHtmlStart(
        nextText.slice(
          header.indent +
            (nextText.startsWith(' '.repeat(header.indent + 2)) ? 2 : 0),
        ),
        false,
      )
    );
  const input = { lines, index, context, line, header };
  const namedType = isNamed ? header.type : undefined;

  if (rest) {
    return readOneLine(input, namedType);
  }

  return namedType !== undefined
    ? readNamed(input, namedType)
    : readInferred(input);
}

/**
 * builds a one-line whole-target annotation
 * @param input opening context
 * @param namedType recognized custom classification, absent for an inferred target
 * @returns the complete annotated or named block
 */
function readOneLine(
  input: BlockInput,
  namedType: string | undefined,
): BlockResult {
  const { lines: _lines, index, context, line, header } = input;
  const closing = findClosingIndex(line.view.text, header.content);

  if (closing < 0) {
    throw new ParseError(
      'MDC_CLOSING_MARKER_INVALID',
      'One-line block annotations require a terminal ;;.',
      context.locate(line.view.mapOffset(0), line.end).start,
    );
  }

  assertCloser(line.view.text.slice(closing), header, {
    context,
    view: line.view,
  });
  const body = sliceView(line.view, header.content, closing);
  const trimmed = sliceView(body, 0, trimSpaceEnd(body.text).length);
  const events = recognizeBlocks(
    [{ view: trimmed, end: trimmed.mapOffset(trimmed.text.length) }],
    context,
  );

  if (
    namedType !== undefined &&
    !(
      header.type === 'html' &&
      events.length === 1 &&
      events[0]?.type === 'html'
    )
  ) {
    return {
      event: namedEvent(
        { type: namedType, body: trimmed, children: events, view: line.view },
        header,
      ),
      next: index + 1,
    };
  }

  const event = events.length === 1 ? events[0] : undefined;

  if (!event) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'One-line annotation must contain one complete target.',
    );
  }

  return {
    event: {
      ...annotateTarget(event, input, true),
      body: trimmed,
      selector: header.type,
      view: line.view,
    },
    next: index + 1,
  };
}

/**
 * gathers the indentation-owned body of a named application block
 * @param input opening context
 * @param type recognized named classification
 * @returns the complete target and its next source line
 */
function readNamed(input: BlockInput, type: string): BlockResult {
  const { lines, index, context, header } = input;
  const owned = readOwnedBody(input);
  const target = lines[index + 1];
  if (owned.body.length === 0 && target && trimSpace(target.view.text)) {
    const end = consumeCloser(input, index + 2);
    return {
      event: {
        type,
        isCustom: true,
        body: target.view,
        selector: header.type,
        namespace: header.namespace,
        attributes: header.attributes,
        ref: header.ref,
        view: getExtent(lines, index, end),
      },
      next: end,
    };
  }
  const children = recognizeBlocks(owned.body, context);
  const end = consumeCloser(input, owned.end);

  return {
    event: {
      type,
      isCustom: true,
      body: joinViews(owned.body.map((line) => line.view)),
      selector: header.type,
      namespace: header.namespace,
      attributes: header.attributes,
      ref: header.ref,
      children,
      view: getExtent(lines, index, end),
    },
    next: end,
  };
}

/**
 * annotates one complete native target without changing its structure
 * @param input opening context
 * @returns the complete target and its next source line
 */
function readInferred(input: BlockInput): BlockResult {
  const { lines, index, context, header } = input;
  const { index: target, line: targetLine } = findInferredTarget(input);
  const isIndented = targetLine.view.text.startsWith(
    ' '.repeat(header.indent + 2),
  );
  const owned = isIndented ? readOwnedBody(input, false) : undefined;
  const targetLines = owned?.body ?? readHtmlAnnotationLines(input, target, targetLine);
  // A referenced `---` target is the package-neutral container sentinel used
  // by compact adapters. It owns the following indented body and is not a
  // thematic-break child of that body.
  const hadSentinel = targetLines.some((line) => trimSpace(line.view.text) === '---');
  const bodyLines = targetLines[0] && trimSpace(targetLines[0].view.text) === '---'
    ? targetLines.slice(1)
    : targetLines;
  const remaining = recognizeBlocks(
    bodyLines,
    context,
    owned ? undefined : 1,
  );
  const isMultiContainer =
    header.type === undefined &&
    remaining.length > 1 &&
    bodyLines.some((line) => trimSpace(line.view.text) === '---') &&
    bodyLines.some((line) => trimSpace(line.view.text).startsWith('::'));
  const first = remaining[0]!;

  if (remaining.length === 0) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'Native annotation body must contain one complete target.',
    );
  }

  const targetEnd = remaining.reduce(
    (end, child) => Math.max(end, child.view.mapOffset(child.view.text.length)),
    targetLine.view.mapOffset(targetLine.view.text.length),
  );
  let end = owned?.end ?? target;

  for (
    let current = lines[end];
    current && current.view.mapOffset(0) <= targetEnd;
    current = lines[end]
  ) {
    end++;
  }

  const event =
    isMultiContainer
      ? {
          type: 'container',
          isCustom: true,
          body: joinViews(bodyLines.map((line) => line.view)),
          selector: header.type,
          namespace: header.namespace,
          attributes: header.attributes,
          ref: header.ref,
          children: remaining,
          view: getExtent(lines, index, end),
        }
      : hadSentinel &&
          first.ref !== undefined &&
          header.ref !== undefined &&
          first.ref !== header.ref
        ? { ...first, ref: header.ref }
        : first.ref !== undefined
          ? first
        : owned && isListEvent(first) && !header.type && !header.namespace
          ? annotateEvent(first, header, context)
          : annotateTarget(first, input);
  end = consumeCloser(input, end);

  return {
    event: {
      ...event,
      body: owned && hadSentinel
        ? joinViews(targetLines.map((line) => line.view))
        : first?.view ?? joinViews(bodyLines.map((line) => line.view)),
      selector: header.type,
      view: getExtent(lines, index, end),
    },
    next: end,
  };
}

/**
 * locates an adjacent native target or the qualified HTML body
 * @param input opening controls and physical source lines
 * @returns the existing adjacent target line and index
 * @throws {ParseError} when the annotation has no adjacent native target
 */
function findInferredTarget(input: BlockInput): {
  index: number;
  line: SourceLine;
} {
  const { lines, index, context, line, header } = input;
  let target = index + 1;

  // qualified HTML owns its literal body; inferred annotations instead need
  // an adjacent target and cannot attach metadata across a blank separator
  if (header.type === 'html') {
    while (target < lines.length && !trimSpace(lines[target]?.view.text)) {
      target++;
    }
  }

  while (target < lines.length && !trimSpace(lines[target]?.view.text)) target++;
  const targetLine = lines[target];

  if (!targetLine || !trimSpace(targetLine.view.text)) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'Native annotation requires a complete target.',
      context.locate(line.view.mapOffset(0), line.end).start,
    );
  }

  return { index: target, line: targetLine };
}

/**
 * applies metadata to the complete target or its selected structural item
 * @param event recognized native target
 * @param input opening controls and shared source coordinates
 * @param shouldMergeList whether adjacent one-line items retain their list group
 * @returns annotated event with the same structural ownership
 */
function annotateTarget(
  event: BlockEvent,
  input: BlockInput,
  shouldMergeList = false,
): BlockEvent {
  const { header, context } = input;
  const expected =
    header.type && (STRUCTURAL_TYPES[header.type] ?? header.type);

  if (
    isListEvent(event) &&
    (expected === 'listItem' || (!expected && !header.namespace))
  ) {
    return {
      ...event,
      shouldMergeList,
      children: event.children.map((item, index) =>
        index === 0
          ? annotateEvent(item, { ...header, type: undefined }, context)
          : item,
      ),
    };
  }

  return annotateEvent(event, header, context);
}

/**
 * validates an aligned local closer after target ownership is established
 * @param input opening context
 * @param end first line outside the owned body
 * @returns first line after a consumed closer, otherwise the body boundary
 */
function consumeCloser(input: BlockInput, end: number): number {
  const { lines, header, context } = input;
  let closingLine = end;

  while (
    closingLine < lines.length &&
    !trimSpace(lines[closingLine]?.view.text)
  ) {
    closingLine++;
  }

  const closing = lines[closingLine];

  if (closing?.view.text.startsWith(' '.repeat(header.indent) + ';;')) {
    // A parent container's aligned closer can follow an inferred child whose
    // body had no explicit closer. Leave it for the parent rather than
    // treating the parent's reference as a mismatch for the child.
    const closerHeader = readHeader(
      sliceView(closing.view, header.indent),
      context,
    );
    if (
      header.ref !== undefined &&
      closerHeader?.ref !== undefined &&
      closerHeader.ref !== header.ref
    ) {
      return end;
    }
    assertCloser(closing.view.text.slice(header.indent), header, {
      context,
      view: closing.view,
    });

    return closingLine + 1;
  }

  return end;
}

/**
 * bounds an unindented HTML target at the annotation's aligned closing control
 * @param input complete opening context
 * @param target first target line
 * @param targetLine existing target source line
 * @returns original mapped lines available to the native recognizer
 */
function readHtmlAnnotationLines(
  input: BlockInput,
  target: number,
  targetLine: SourceLine,
): readonly SourceLine[] {
  const { lines, header } = input;

  if (!readHtmlStart(targetLine.view.text, false)) {
    return lines.slice(target);
  }

  // only the owning annotation recognizes this control; ordinary CommonMark
  // HTML remains opaque, and indented body markers remain literal HTML bytes
  let end = target;
  const prefix = ' '.repeat(header.indent) + ';;';

  for (let current = lines[end]; current; current = lines[end]) {
    const text = current.view.text;

    if (
      text.startsWith(prefix) &&
      /^(?:[ \t]*#\w[A-Za-z0-9_-]*)?[ \t]*$/.test(text.slice(prefix.length))
    ) {
      break;
    }

    end++;
  }

  return lines.slice(target, end);
}

/**
 * retains a named lexical body and its authoritative source selectors
 * @param body resolved semantic body event
 * @param header original opening selectors
 * @returns complete lexical application event
 */
function namedEvent(body: BlockEvent, header: BlockHeader): BlockEvent {
  return {
    ...body,
    isCustom: true,
    selector: header.type,
    namespace: header.namespace,
    attributes: header.attributes,
    ref: header.ref,
  };
}
