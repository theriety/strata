import { ParseError } from '#errors';

import { trimSpace } from './whitespace';

import type { ParseOptions } from '#parser/types';

import type { SourcePosition } from '#types';

import type {
  SourceContext,
  SourceView,
  SourceLine,
  ParseBudget,
} from './types';

const BOUNDARY_BYTES = Float64Array.BYTES_PER_ELEMENT;

/**
 * preserves each original UTF-16 boundary
 * @param source original source text
 * @returns the resolved mapped source
 */
export function createSourceView(source: string): SourceView {
  return createMappedView(
    source.replace(/\0/g, '\ufffd'),
    Array.from({ length: source.length + 1 }, (_, index) => index),
  );
}

/**
 * splits physical lines while retaining CRLF and CR source widths
 * @param view mapped source region
 * @returns the resolved physical lines
 */
export function getSourceLines(view: SourceView): SourceLine[] {
  const lines: SourceLine[] = [];
  let start = 0;

  for (let index = 0; index < view.text.length; index++) {
    if (view.text[index] !== '\r' && view.text[index] !== '\n') {
      continue;
    }

    const end = index;

    if (view.text[index] === '\r' && view.text[index + 1] === '\n') {
      index++;
    }

    lines.push({
      view: sliceView(view, start, end),
      end: view.mapOffset(index + 1),
    });
    start = index + 1;
  }

  if (start < view.text.length) {
    lines.push({
      view: sliceView(view, start),
      end: view.mapOffset(view.text.length),
    });
  }

  return lines;
}

/**
 * slices a mapped source region
 * @param view mapped source region
 * @param start inclusive region boundary
 * @param end exclusive source boundary
 * @returns the resolved source slice
 */
export function sliceView(
  view: SourceView,
  start: number,
  end = view.text.length,
): SourceView {
  const text = view.text.slice(start, end);

  return createMappedView(
    text,
    Array.from({ length: text.length + 1 }, (_, index) =>
      view.mapOffset(start + index),
    ),
  );
}

/**
 * combines whole-node source spans across physical lines
 * @param lines physical source lines
 * @param start inclusive region boundary
 * @param end exclusive region boundary
 * @returns the resolved whole-node source view
 */
export function getExtent(
  lines: readonly SourceLine[],
  start: number,
  end: number,
): SourceView {
  let boundary = end;

  while (boundary > start && !trimSpace(lines[boundary - 1]?.view.text)) {
    boundary--;
  }

  return joinViews(lines.slice(start, boundary).map((line) => line.view));
}

/**
 * joins mapped regions with normalized syntax line endings
 * @param views mapped source regions
 * @param separator syntax separator between mapped regions
 * @returns the resolved joined source mapping
 */
export function joinViews(
  views: readonly SourceView[],
  separator = '\n',
): SourceView {
  let text = '';
  const offsets: number[] = [];
  let boundary = 0;

  for (const [index, view] of views.entries()) {
    if (index > 0) {
      for (let remaining = separator.length; remaining > 0; remaining--) {
        offsets.push(boundary);
      }

      text += separator;
    }

    for (let index = 0; index < view.text.length; index++) {
      offsets.push(view.mapOffset(index));
    }

    boundary = view.mapOffset(view.text.length);
    text += view.text;
  }

  offsets.push(boundary);

  return createMappedView(text, offsets);
}

/**
 * stores exactly one original offset for each UTF-16 boundary
 * @param text mapped syntax text
 * @param offsets complete boundary sequence from a source producer
 * @returns mapped text with constant-time boundary lookup
 */
export function createMappedView(
  text: string,
  offsets: Iterable<number>,
): SourceView {
  return { text, mapOffset: createBoundaryLookup(offsets) };
}

/**
 * builds the one shared source and nesting context
 * @param source original source text
 * @param options parser configuration
 * @returns the resolved document context
 */
export function createContext(
  source: string,
  options: ParseOptions,
): SourceContext {
  const getPosition = createPosition(source);

  return {
    source,
    options,
    definitions: new Map(),
    locate: (start, end) => ({
      start: getPosition(start),
      end: getPosition(end),
    }),
    budget: {
      depth: 0,
      limit: 128,
      /** enters one shared nesting level */
      enter(this: ParseBudget) {
        if (this.depth >= this.limit) {
          throw new ParseError(
            'MDC_RESOURCE_LIMIT',
            'MDCX nesting limit of 128 exceeded.',
          );
        }

        this.depth++;
      },
      /** releases one shared nesting level */
      leave(this: ParseBudget) {
        this.depth--;
      },
    },
  };
}

/**
 * indexes original line endings for UTF-16 source coordinates
 * @param source original source text
 * @returns a point resolver sharing one line-start index
 */
export function createPosition(
  source: string,
): (offset: number) => SourcePosition {
  const starts = [0];

  for (let index = 0; index < source.length; index++) {
    if (source[index] === '\r') {
      if (source[index + 1] === '\n') {
        index++;
      }

      starts.push(index + 1);
    } else if (source[index] === '\n') {
      starts.push(index + 1);
    }
  }

  const getLineStart = createBoundaryLookup(starts);
  const lineCount = starts.length;

  return (offset: number): SourcePosition => {
    let low = 0;
    let high = lineCount;

    while (low + 1 < high) {
      const middle = (low + high) >>> 1;

      if (getLineStart(middle) <= offset) {
        low = middle;
      } else {
        high = middle;
      }
    }

    return { line: low + 1, column: offset - getLineStart(low) + 1, offset };
  };
}

/**
 * stores a dense immutable index without inventing missing boundary values
 * @param offsets integer source boundaries in producer order
 * @returns constant-time lookup for a valid integer index
 */
function createBoundaryLookup(
  offsets: Iterable<number>,
): (index: number) => number {
  const boundaries = Array.from(offsets);
  const mapping = new DataView(
    new ArrayBuffer(boundaries.length * BOUNDARY_BYTES),
  );
  let index = 0;

  for (const offset of boundaries) {
    mapping.setFloat64(index * BOUNDARY_BYTES, offset, true);
    index++;
  }

  // all internal cursors are integer UTF-16 boundaries. DataView retains
  // bounds errors rather than substituting an unrelated original offset.
  return (boundary) => mapping.getFloat64(boundary * BOUNDARY_BYTES, true);
}
