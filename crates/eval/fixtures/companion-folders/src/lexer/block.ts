import { ParseError } from '#errors';

import { readHeading, readIndented, readParagraph } from './block/leaf';

import { readHeader, readMdcxBlock } from './block/mdcx';

import { readFence, readHtmlStart, readHtml } from './block/opaque';

import { readTable } from './block/table';
import { readList, readQuote } from './container';
import { readDefinition } from './definition';
import { joinViews } from './source';

import { trimSpace } from './whitespace';

import type {
  BlockEvent,
  BlockSource,
  SourceContext,
  SourceLine,
  SourceView,
} from './types';

/** recognized block and the next unconsumed physical line */
export interface BlockResult {
  event: BlockEvent;
  next: number;
}

/** active opaque fenced-code context */
export interface FenceState {
  marker: string;
  length: number;
  openingIndent: number;
}

/**
 * resolves flow precedence and container ownership before inline parsing
 * @param lines physical source lines
 * @param context shared parsing and source context
 * @param maximum maximum number of resolved blocks
 * @returns the resolved block syntax events
 */
export function recognizeBlocks(
  lines: readonly SourceLine[],
  context: SourceContext,
  maximum = Infinity,
): BlockEvent[] {
  context.budget.enter();

  try {
    const events: BlockEvent[] = [];
    let index = 0;

    for (
      let line = lines[index];
      line && events.length < maximum;
      line = lines[index]
    ) {
      const text = line.view.text;

      if (!trimSpace(text)) {
        index++;
        continue;
      }

      if (discardOrphan(line, context)) {
        index++;
        continue;
      }

      const block = readBlock({ lines, index, context, line });
      events.push(block.event);
      index = block.next;
    }

    return groupItems(events);
  } finally {
    context.budget.leave();
  }
}

/**
 * tests constructs permitted to interrupt an open paragraph
 * @param view current line with original boundary mapping
 * @param context shared parsing and source context
 * @returns the resolved paragraph interruption classification
 */
export function canInterruptParagraph(
  view: SourceView,
  context: SourceContext,
): boolean {
  const { text } = view;

  if (
    /^ {0,3}(?:#{1,6}(?:[ \t]|$)|>|`{3,}|~{3,})/.test(text) ||
    isThematic(text)
  ) {
    return true;
  }

  if (/^ {0,3}(?:[-+*]|1[.)])[ \t]+\S/.test(text)) {
    return true;
  }

  if (
    context.options.profile !== 'commonmark' &&
    (/^ {0,3};;/.test(text) || readHeader(view, context))
  ) {
    return true;
  }

  return readHtmlStart(text, true) !== null;
}

/**
 * selects the first legal block production in grammar precedence
 * @param source existing current line and its shared source context
 * @returns the resolved block and next source boundary
 */
function readBlock(source: BlockSource): BlockResult {
  const { lines, index, context, line } = source;
  const hasExtensions = context.options.profile !== 'commonmark';

  return (
    readDefinition(lines, index, context) ??
    (hasExtensions ? readTable(lines, index, context) : null) ??
    (hasExtensions ? readMath(line, index) : null) ??
    readList(source) ??
    (hasExtensions ? readMdcxBlock(source) : null) ??
    readFence(source) ??
    readHeading(line, index) ??
    (isThematic(line.view.text)
      ? { event: { type: 'thematicBreak', view: line.view }, next: index + 1 }
      : null) ??
    readQuote(lines, index, context) ??
    readHtml(source, false) ??
    readIndented(lines, index, line) ??
    readParagraph(source)
  );
}

/**
 * tests thematic-break precedence without accepting mixed markers
 * @param text current line text
 * @returns the resolved thematic-break classification
 */
export function isThematic(text: string): boolean {
  if (/^ {4}|^ {0,3}\t/.test(text)) {
    return false;
  }

  const input = text.replace(/^ {0,3}/, '').replace(/[ \t]/g, '');

  return /^(?:\*{3,}|_{3,}|-{3,})$/.test(input);
}

/**
 * recognizes the approved whole-line math literal grammar
 * @param line current physical source line
 * @param index current source line index
 * @returns positioned literal math or null
 */
function readMath(line: SourceLine, index: number): BlockResult | null {
  if (!/^ {0,3}\$\$/.test(line.view.text)) {
    return null;
  }

  const text = trimSpace(line.view.text);

  if (
    !text.startsWith('$$') ||
    !text.endsWith('$$') ||
    text.length < '$$$$'.length
  ) {
    return null;
  }

  return {
    event: {
      type: 'math',
      value: text.slice('$$'.length, -'$$'.length),
      view: line.view,
    },
    next: index + 1,
  };
}

/**
 * validates orphan controls and performs the one authorized recovery
 * @param line current physical source line
 * @param context shared source and parsing context
 * @returns whether a recovered orphan consumed this line
 */
function discardOrphan(line: SourceLine, context: SourceContext): boolean {
  if (
    context.options.profile === 'commonmark' ||
    !/^ {0,3};;/.test(line.view.text)
  ) {
    return false;
  }

  const range = context.locate(line.view.mapOffset(0), line.end);
  const text = trimSpace(line.view.text);

  if (
    context.options.mode === 'recover' &&
    /^(?:#\w[A-Za-z0-9_-]*)?$/.test(trimSpace(text.slice(2)))
  ) {
    context.options.onDiagnostic({
      code: 'MDC_CLOSING_MARKER_INVALID',
      severity: 'warning',
      message:
        'Discarded an orphan closing marker; no document identity or hierarchy was changed.',
      range,
    });

    return true;
  }

  throw new ParseError(
    'MDC_CLOSING_MARKER_INVALID',
    'Closing marker has no owning block.',
    range.start,
  );
}

/**
 * joins item-only annotation events without splitting the owning list
 * @param events resolved block syntax events
 * @returns grouped block events
 */
function groupItems(events: readonly BlockEvent[]): BlockEvent[] {
  const grouped: BlockEvent[] = [];

  for (const event of events) {
    const previous = grouped.at(-1);

    if (
      isListEvent(previous) &&
      previous.shouldMergeList &&
      isListEvent(event) &&
      previous.listMarker === event.listMarker
    ) {
      grouped[grouped.length - 1] = {
        ...previous,
        children: [...previous.children, ...event.children],
        spread: Boolean(previous.spread) || Boolean(event.spread),
        view: joinViews([previous.view, event.view]),
        shouldMergeList: event.shouldMergeList,
      };
    } else {
      grouped.push(event);
    }
  }

  return grouped;
}

/**
 * narrows a list event only when its structural descendants are present
 * @param event candidate syntax event
 * @returns whether the event has a complete list child collection
 */
export function isListEvent(
  event: BlockEvent | undefined,
): event is BlockEvent & { children: BlockEvent[] } {
  return event?.type === 'list' && event.children !== undefined;
}
