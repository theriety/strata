import { skipInlineOpaque } from '../boundary';
import { createMappedView, getExtent, sliceView } from '../source';

import { trimSpace, trimSpaceStart, trimSpaceEnd } from '../whitespace';

import {
  annotateEvent,
  assertCloser,
  findClosingIndex,
  readHeader,
} from './mdcx';

import type { BlockResult } from '../block';

import type {
  BlockEvent,
  SourceContext,
  SourceLine,
  SourceView,
} from '../types';

/**
 * Recognizes header tables and MDCX headerless tables using complete row units.
 * @param lines mapped physical source lines
 * @param index first row or its preceding annotation
 * @param context shared parser configuration
 * @returns table event and first unconsumed line, or null
 */
export function readTable(
  lines: readonly SourceLine[],
  index: number,
  context: SourceContext,
): BlockResult | null {
  const first = readTableRow(lines, index, context);
  if (!first) return null;

  const cells = splitCells(first.view);
  const separator = lines[first.next];
  const alignCells = separator ? splitCells(separator.view) : [];
  const hasHeader = alignCells.length === cells.length &&
    alignCells.length > 0 &&
    alignCells.every((part) => /^:?-+:?$/.test(part.text));

  if (!hasHeader && (context.options.profile === 'commonmark' ||
    !trimSpace(first.view.text).startsWith('|') ||
    !trimSpace(first.view.text).endsWith('|'))) return null;
  // A headerless table requires at least two complete rows. A single pipe
  // line remains ordinary paragraph content or a custom adapter body.
  if (!hasHeader && (cells.length < 2 || !readTableRow(lines, first.next, context))) return null;

  const align = hasHeader ? alignCells.map((part) =>
    part.text.startsWith(':')
      ? part.text.endsWith(':') ? ('center' as const) : ('left' as const)
      : part.text.endsWith(':') ? ('right' as const) : null,
  ) : cells.map(() => null);
  const rows: BlockEvent[] = [];
  const append = (row: TableRowInput): void => {
    const event = createRow(row.view, cells.length, context);
    rows.push(row.header ? annotateEvent(event, row.header, context) : event);
  };
  append(first);
  let end = first.next + (hasHeader ? 1 : 0);
  while (end < lines.length) {
    const row = readTableRow(lines, end, context);
    if (!row || (!hasHeader && splitCells(row.view).length !== cells.length)) break;
    append(row);
    end = row.next;
  }
  return {
    event: { type: 'table', hasHeader, align, children: rows, view: getExtent(lines, index, end) },
    next: end,
  };
}

interface TableRowInput {
  view: SourceView;
  header: ReturnType<typeof readHeader>;
  next: number;
}

/**
 * Reads an optional structural annotation and its adjacent pipe row.
 * Named application envelopes and indented child bodies keep their own owners.
 * @param lines mapped source lines
 * @param index candidate row index
 * @param context shared parsing configuration
 * @returns row body with its metadata and boundary, or null
 */
function readTableRow(
  lines: readonly SourceLine[], index: number, context: SourceContext,
): TableRowInput | null {
  const line = lines[index];
  if (!line || !trimSpace(line.view.text)) return null;
  let view = line.view;
  let header = readHeader(view, context);
  let next = index + 1;
  if (header) {
    if (context.options.profile === 'commonmark' ||
      (header.namespace && header.namespace !== 'common') ||
      (header.type && header.type !== 'row')) return null;
    if (trimSpace(view.text.slice(header.content))) {
      const inline = readRowView(view, context);
      view = inline.view;
    } else {
      const body = lines[next];
      if (!body || body.view.text.search(/\S/) !== header.indent) return null;
      if (!body.view.text.includes('|')) return null;
      view = body.view;
      next++;
      const closing = findClosingIndex(view.text, 0);
      if (closing >= 0) {
        assertCloser(view.text.slice(closing), header, { context, view });
        view = sliceView(view, 0, closing);
      }
    }
    const closer = lines[next];
    if (closer && closer.view.text.search(/\S/) === header.indent &&
      trimSpace(closer.view.text).startsWith(';;')) {
      assertCloser(trimSpace(closer.view.text), header, { context, view: closer.view });
      next++;
    }
  }
  if (!view.text.includes('|')) return null;
  return { view, header, next };
}

/**
 * constructs restricted row and cell syntax events
 * @param view mapped source region
 * @param width table column count
 * @param context shared source and parsing context
 * @returns the resolved restricted row syntax
 */
function createRow(
  view: SourceView,
  width: number,
  context: SourceContext,
): BlockEvent {
  const target = readRowView(view, context);
  const content = splitCells(target.view);
  const children: BlockEvent[] = [];

  for (let index = 0; index < width; index++) {
    const part = content[index] ?? sliceView(view, view.text.length);
    const header = readHeader(part, context);
    let event: BlockEvent & { inline: SourceView } = {
      type: 'tableCell',
      inline: part,
      view: part,
    };

    if (header) {
      const closing = findClosingIndex(part.text, header.content);
      const end = closing < 0 ? part.text.length : closing;

      if (closing >= 0) {
        assertCloser(part.text.slice(end), header, { context, view: part });
      }
      const body = sliceView(part, header.content, end);
      event = annotateEvent(
        {
          ...event,
          inline: sliceView(body, 0, trimSpaceEnd(body.text).length),
        },
        header,
        context,
      );
    }

    event = { ...event, inline: decodeCellPipes(event.inline) };
    children.push(event);
  }

  const event: BlockEvent = { type: 'tableRow', children, view };

  return target.header ? annotateEvent(event, target.header, context) : event;
}

/**
 * splits GFM cells without treating escaped or opaque pipes as delimiters
 * @param view mapped source region
 * @returns the resolved mapped cell regions
 */
function splitCells(view: SourceView): SourceView[] {
  const result: SourceView[] = [];
  let start = 0;
  let brackets = 0;
  let state = { index: 0, ticks: 0 };

  while (state.index < view.text.length) {
    const opaque = skipInlineOpaque(view.text, state.index, state.ticks);

    if (opaque) {
      state = opaque;
      continue;
    }

    const index = state.index;
    const character = view.text[index];

    if (character === '[') {
      brackets++;
    }

    if (character === ']') {
      brackets = Math.max(0, brackets - 1);
    }

    if (character === '|' && !brackets) {
      result.push(sliceView(view, start, index));
      start = index + 1;
    }

    state.index++;
  }

  result.push(sliceView(view, start));

  if (!trimSpace(result[0]?.text)) {
    result.shift();
  }

  if (!trimSpace(result.at(-1)?.text)) {
    result.pop();
  }

  return result.map((part) => {
    const start = part.text.length - trimSpaceStart(part.text).length;

    return sliceView(part, start, trimSpaceEnd(part.text).length);
  });
}

/**
 * recognizes metadata on a complete table row
 * @param view mapped source region
 * @param context shared source and parsing context
 * @returns the resolved row body and annotation
 */
function readRowView(
  view: SourceView,
  context: SourceContext,
): {
  view: SourceView;
  header: ReturnType<typeof readHeader>;
} {
  const header = readHeader(view, context);

  if (!header || !view.text.slice(header.content).includes('|')) {
    return { view, header: null };
  }

  const closing = findClosingIndex(view.text, header.content);
  const end = closing < 0 ? view.text.length : closing;

  if (closing >= 0) {
    assertCloser(view.text.slice(end), header, { context, view });
  }

  return { view: sliceView(view, header.content, end), header };
}

/**
 * removes GFM pipe escaping before inline code opacity applies
 * @param view mapped cell region
 * @returns mapped content with one table escape layer removed
 */
function decodeCellPipes(view: SourceView): SourceView {
  const characters: string[] = [];
  const offsets: number[] = [];
  let index = -1;

  while (index + 1 < view.text.length) {
    index++;
    offsets.push(view.mapOffset(index));

    if (view.text[index] === '\\' && view.text[index + 1] === '|') {
      index++;
    }

    characters.push(view.text.charAt(index));
  }

  offsets.push(view.mapOffset(view.text.length));

  return createMappedView(characters.join(''), offsets);
}
