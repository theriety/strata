import type { SourceView } from '#lexer/types';

import type { SourceRange } from '#types';

import type { InlineCoordinates, InlineEnvironment } from './types';

/** temporary syntax sequence; never a second semantic tree */
export interface InlineSequence<Node> {
  first?: InlineToken<Node>;
  last?: InlineToken<Node>;
  delimiter?: Delimiter<Node>;
}

/** every positioned event is either literal syntax or resolved output */
export type InlineToken<Node> = LiteralToken<Node> | NodeToken<Node>;

/** emphasis delimiter state referring to source events */
export interface Delimiter<Node> {
  token: LiteralToken<Node>;
  marker: string;
  length: number;
  canOpen: boolean;
  canClose: boolean;
  previous?: Delimiter<Node>;
  next?: Delimiter<Node>;
}

/** unresolved literal syntax, including mutable delimiter runs */
export interface LiteralToken<Node> extends TokenBoundary<Node> {
  value: string;
  node?: never;
}

/** resolved semantic output retained in the local token sequence */
interface NodeToken<Node> extends TokenBoundary<Node> {
  node: Node;
  value?: never;
}

/** resolved inline syntax event in a mutable local linked sequence */
interface TokenBoundary<Node> {
  start: number;
  end: number;
  previous?: InlineToken<Node>;
  next?: InlineToken<Node>;
}

interface EmphasisPair<Node> extends InlineEnvironment<Node> {
  sequence: InlineSequence<Node>;
  opener: Delimiter<Node>;
  closer: Delimiter<Node>;
}

const DELIMITER_MODULUS = 3;

/**
 * appends one positioned syntax event
 * @param sequence active local sequence
 * @param token event to append
 * @returns appended event
 */
export function appendToken<Node, Token extends InlineToken<Node>>(
  sequence: InlineSequence<Node>,
  token: Token,
): Token {
  token.previous = sequence.last;

  if (sequence.last) {
    sequence.last.next = token;
  } else {
    sequence.first = token;
  }

  sequence.last = token;

  return token;
}

/**
 * resolves delimiter pairs within a bracket or complete inline region
 * @param sequence local event sequence
 * @param bottom delimiter preceding the region
 * @param environment mapped source and shared document context
 */
export function resolveEmphasis<Node>(
  sequence: InlineSequence<Node>,
  bottom: Delimiter<Node> | undefined,
  environment: InlineEnvironment<Node>,
): void {
  let closer = bottom ? bottom.next : findFirstDelimiter(sequence.delimiter);
  const bottoms = new Map<string, Delimiter<Node> | undefined>();

  while (closer) {
    if (!closer.canClose) {
      closer = closer.next;
      continue;
    }

    const key = `${closer.marker}:${closer.canOpen}:${closer.length % DELIMITER_MODULUS}`;
    const floor = bottoms.has(key) ? bottoms.get(key) : bottom;
    const opener = findOpener(closer, floor, bottom);

    if (!opener || opener === floor || opener === bottom) {
      bottoms.set(key, closer.previous);
      const next = closer.next;

      if (!closer.canOpen) {
        removeDelimiter(sequence, closer);
      }

      closer = next;
      continue;
    }

    closer = joinEmphasis({ ...environment, sequence, opener, closer });
  }

  while (sequence.delimiter && sequence.delimiter !== bottom) {
    removeDelimiter(sequence, sequence.delimiter);
  }
}

/**
 * removes events surrounding a resolved bracket construct
 * @param sequence active local sequence
 * @param opener opening bracket event
 * @param replacement resolved construct
 */
export function replaceBracket<Node>(
  sequence: InlineSequence<Node>,
  opener: InlineToken<Node>,
  replacement: InlineToken<Node>,
): void {
  replacement.previous = opener.previous;

  if (opener.previous) {
    opener.previous.next = replacement;
  } else {
    sequence.first = replacement;
  }

  sequence.last = replacement;
}

/**
 * selects an eligible opener without rescanning exhausted delimiter regions
 * @param closer closing delimiter
 * @param floor last failed-search boundary
 * @param bottom containing bracket boundary
 * @returns matching opener when available
 */
function findOpener<Node>(
  closer: Delimiter<Node>,
  floor: Delimiter<Node> | undefined,
  bottom: Delimiter<Node> | undefined,
): Delimiter<Node> | undefined {
  let opener = closer.previous;

  while (opener && opener !== floor && opener !== bottom) {
    if (isEligiblePair(opener, closer)) {
      return opener;
    }

    opener = opener.previous;
  }

  return undefined;
}

/**
 * checks marker compatibility and the multiple-of-three rule
 * @param opener prospective opening run
 * @param closer prospective closing run
 * @returns whether the runs may form emphasis
 */
function isEligiblePair<Node>(
  opener: Delimiter<Node>,
  closer: Delimiter<Node>,
): boolean {
  const isOddMatch =
    (opener.canClose || closer.canOpen) &&
    (opener.length + closer.length) % DELIMITER_MODULUS === 0 &&
    (opener.length % DELIMITER_MODULUS !== 0 ||
      closer.length % DELIMITER_MODULUS !== 0);

  return (
    opener.marker === closer.marker &&
    opener.canOpen &&
    !isOddMatch &&
    (closer.marker !== '~' || opener.length === closer.length)
  );
}

/**
 * replaces one matched delimiter pair with sink-selected parent output
 * @param pair matched runs and source environment
 * @returns next closing delimiter to resolve
 */
function joinEmphasis<Node>(
  pair: EmphasisPair<Node>,
): Delimiter<Node> | undefined {
  const { sequence, opener, closer, view, context } = pair;
  const count =
    closer.marker === '~'
      ? closer.length
      : opener.length >= 2 && closer.length >= 2
        ? 2
        : 1;
  const children = buildChildren(opener.token.next, closer.token, pair);
  const boundary = {
    start: opener.token.end - count,
    end: closer.token.start + count,
  };
  const range = getTokenRange(boundary, view, context);
  const wrapper: InlineToken<Node> = {
    ...boundary,
    previous: opener.token,
    next: closer.token,
    node: pair.sink.createParent(
      closer.marker === '~' ? 'delete' : count === 2 ? 'strong' : 'emphasis',
      children,
      range,
    ),
  };
  opener.token.next = wrapper;
  closer.token.previous = wrapper;
  let inner = opener.next;

  while (inner && inner !== closer) {
    const next = inner.next;
    removeDelimiter(sequence, inner);
    inner = next;
  }

  opener.length -= count;
  opener.token.end -= count;
  opener.token.value = opener.marker.repeat(opener.length);
  closer.length -= count;
  closer.token.start += count;
  closer.token.value = closer.marker.repeat(closer.length);

  if (opener.length === 0) {
    removeToken(sequence, opener.token);
    removeDelimiter(sequence, opener);
  }

  if (closer.length === 0) {
    const next = closer.next;
    removeToken(sequence, closer.token);
    removeDelimiter(sequence, closer);

    return next;
  }

  return closer;
}

/**
 * converts resolved events through the sink, which owns output and coalescing
 * @param first first event
 * @param stop exclusive sequence boundary
 * @param environment mapped source and shared document context
 * @returns sink-selected output values
 */
export function buildChildren<Node>(
  first: InlineToken<Node> | undefined,
  stop: InlineToken<Node> | undefined,
  environment: InlineEnvironment<Node>,
): Node[] {
  const { view, context } = environment;
  const children: Node[] = [];
  let current = first;

  while (current && current !== stop) {
    const range = getTokenRange(current, view, context);

    if (current.value !== undefined) {
      if (current.value) {
        const node = environment.sink.createLiteral(
          { type: 'text', value: current.value },
          range,
        );
        environment.sink.append(children, node, true);
      }
    } else {
      environment.sink.append(children, current.node, false);
    }

    current = current.next;
  }

  return children;
}

/**
 * maps a syntax extent into the original document
 * @param token syntax event
 * @param view boundary mapping
 * @param context original coordinate provider
 * @returns original source range
 */
export function getTokenRange<Node>(
  token: Pick<InlineToken<Node>, 'start' | 'end'>,
  view: SourceView,
  context: InlineCoordinates,
): SourceRange {
  return context.locate(view.mapOffset(token.start), view.mapOffset(token.end));
}

/**
 * finds the beginning of a delimiter chain
 * @param last last delimiter in the chain
 * @returns first delimiter, or undefined for an empty chain
 */
function findFirstDelimiter<Node>(
  last: Delimiter<Node> | undefined,
): Delimiter<Node> | undefined {
  let first = last;

  while (first?.previous) {
    first = first.previous;
  }

  return first;
}

/**
 * unlinks a resolved delimiter from its local chain
 * @param sequence active syntax sequence
 * @param delimiter delimiter to unlink
 */
function removeDelimiter<Node>(
  sequence: InlineSequence<Node>,
  delimiter: Delimiter<Node>,
): void {
  if (delimiter.previous) {
    delimiter.previous.next = delimiter.next;
  }

  if (delimiter.next) {
    delimiter.next.previous = delimiter.previous;
  }

  if (sequence.delimiter === delimiter) {
    sequence.delimiter = delimiter.previous;
  }
}

/**
 * unlinks a consumed syntax marker
 * @param sequence active syntax sequence
 * @param token syntax event to unlink
 */
function removeToken<Node>(
  sequence: InlineSequence<Node>,
  token: InlineToken<Node>,
): void {
  if (token.previous) {
    token.previous.next = token.next;
  } else {
    sequence.first = token.next;
  }

  if (token.next) {
    token.next.previous = token.previous;
  } else {
    sequence.last = token.previous;
  }
}
