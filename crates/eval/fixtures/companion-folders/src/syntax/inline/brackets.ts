import { ParseError } from '#errors';

import { readLinkMatch } from './link';
import {
  appendToken,
  buildChildren,
  replaceBracket,
  resolveEmphasis,
  getTokenRange,
} from './tokens';

import type { SourceView } from '#lexer/types';

import type { Delimiter, InlineSequence, InlineToken } from './tokens';

import type {
  InlineSelector,
  InlineCoordinates,
  InlineEnvironment,
  InlineSink,
  InlineAnnotations,
} from './types';

interface Bracket<Node> {
  token: InlineToken<Node>;
  contentStart: number;
  isImage: boolean;
  isActive: boolean;
  hasBracketAfter: boolean;
  delimiter?: Delimiter<Node>;
  annotation?: {
    selector: InlineSelector;
    adapter: InlineAnnotations<Node>;
  };
  previous?: Bracket<Node>;
}

interface BracketResolverParams<Node> extends InlineEnvironment<Node> {
  sequence: InlineSequence<Node>;
}

/** resolves bracket structure while preserving emphasis region boundaries */
export class BracketResolver<Node> {
  readonly #view: SourceView;
  readonly #context: InlineCoordinates;
  readonly #sink: InlineSink<Node>;
  readonly #definitions: Pick<ReadonlySet<string>, 'has'>;
  readonly #sequence: InlineSequence<Node>;
  #cursor = 0;
  #bracket?: Bracket<Node>;

  /**
   * initializes local bracket ownership
   * @param params shared event sequence and source environment
   */
  constructor(params: BracketResolverParams<Node>) {
    this.#view = params.view;
    this.#context = params.context;
    this.#sequence = params.sequence;
    this.#sink = params.sink;
    this.#definitions = params.definitions;
  }

  /** checks that committed MDCX content did not end without a closer */
  public finish(): void {
    let bracket = this.#bracket;

    while (bracket) {
      if (bracket.annotation) {
        throw this.#createError(
          'Unterminated MDCX inline content.',
          bracket.token.start,
        );
      }

      bracket = bracket.previous;
    }
  }

  /**
   * opens a recognized MDCX content bracket
   * @param start inclusive source boundary
   * @param adapter enabled application annotation semantics
   * @returns cursor after the opener, or undefined when no selector matches
   */
  public openAnnotation(
    start: number,
    adapter: InlineAnnotations<Node>,
  ): number | undefined {
    this.#cursor = start;
    const selector = adapter.open(this.#view.text, this.#cursor);

    if (!selector) {
      return undefined;
    }

    this.#cursor = selector.bracket + 1;
    const token = this.#appendText(
      this.#view.text.slice(start, this.#cursor),
      start,
      this.#cursor,
    );

    if (this.#bracket) {
      this.#bracket.hasBracketAfter = true;
    }

    this.#bracket = {
      token,
      contentStart: this.#cursor,
      isImage: false,
      isActive: true,
      hasBracketAfter: false,
      delimiter: this.#sequence.delimiter,
      annotation: { selector, adapter },
      previous: this.#bracket,
    };

    return this.#cursor;
  }

  /**
   * records an active link or image bracket
   * @param start inclusive source boundary
   * @param isImage whether the bracket introduces an image
   * @returns cursor after the opening bracket
   */
  public open(start: number, isImage: boolean): number {
    this.#cursor = start;
    this.#cursor += isImage ? 2 : 1;
    const token = this.#appendText(isImage ? '![' : '[', start, this.#cursor);

    if (this.#bracket) {
      this.#bracket.hasBracketAfter = true;
    }

    this.#bracket = {
      token,
      contentStart: this.#cursor,
      isImage,
      isActive: true,
      hasBracketAfter: false,
      delimiter: this.#sequence.delimiter,
      previous: this.#bracket,
    };

    return this.#cursor;
  }

  /**
   * resolves the innermost bracket without ancestor searching
   * @param start inclusive source boundary
   * @returns cursor after the resolved resource or literal closing bracket
   */
  public close(start: number): number {
    this.#cursor = start;
    const bracket = this.#bracket;

    if (!bracket) {
      this.#appendText(']', this.#cursor, ++this.#cursor);

      return this.#cursor;
    }

    this.#bracket = bracket.previous;

    if (bracket.annotation) {
      this.#closeAnnotation(bracket, bracket.annotation);

      return this.#cursor;
    }

    const match = bracket.isActive
      ? readLinkMatch({
          source: this.#view.text,
          close: this.#cursor,
          contentStart: bracket.contentStart,
          hasBracketAfter: bracket.hasBracketAfter,
          definitions: this.#definitions,
        })
      : undefined;

    if (!match) {
      this.#appendText(']', this.#cursor, ++this.#cursor);

      return this.#cursor;
    }

    resolveEmphasis(this.#sequence, bracket.delimiter, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });
    const children = buildChildren(bracket.token.next, undefined, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });
    const range = getTokenRange(
      { start: bracket.token.start, end: match.end },
      this.#view,
      this.#context,
    );
    const node = this.#sink.createLink({
      match,
      isImage: bracket.isImage,
      children,
      range,
    });
    const token: InlineToken<Node> = {
      start: bracket.token.start,
      end: match.end,
      node,
    };
    replaceBracket(this.#sequence, bracket.token, token);
    this.#cursor = match.end;

    if (!bracket.isImage) {
      this.deactivateLinks();
    }

    return this.#cursor;
  }

  /** prevents enclosing links from accepting a nested link */
  public deactivateLinks(): void {
    let bracket = this.#bracket;

    while (bracket) {
      if (!bracket.isImage && !bracket.annotation) {
        if (!bracket.isActive) {
          break;
        }

        bracket.isActive = false;
      }

      bracket = bracket.previous;
    }
  }

  /**
   * appends literal syntax markers pending bracket resolution
   * @param value marker spelling
   * @param start inclusive source boundary
   * @param end exclusive source boundary
   * @returns appended marker event
   */
  #appendText(value: string, start: number, end: number): InlineToken<Node> {
    return appendToken(this.#sequence, { value, start, end });
  }

  /**
   * builds a validated inline annotation
   * @param bracket innermost pending bracket
   * @param annotation recognized selector and its owning semantic adapter
   * @param annotation.selector recognized MDCX selector
   * @param annotation.adapter semantic adapter captured at its opener
   */
  #closeAnnotation(
    bracket: Bracket<Node>,
    annotation: { selector: InlineSelector; adapter: InlineAnnotations<Node> },
  ): void {
    resolveEmphasis(this.#sequence, bracket.delimiter, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });
    const children = buildChildren(bracket.token.next, undefined, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });
    const result = annotation.adapter.close({
      selector: annotation.selector,
      children,
      end: this.#cursor + 1,
      range: getTokenRange(
        { start: bracket.token.start, end: this.#cursor + 1 },
        this.#view,
        this.#context,
      ),
    });
    const token: InlineToken<Node> = {
      start: bracket.token.start,
      end: result.end,
      node: result.node,
    };
    replaceBracket(this.#sequence, bracket.token, token);
    this.#cursor = result.end;
  }

  /**
   * creates an actionable original-source diagnostic
   * @param message actionable diagnostic description
   * @param offset source character boundary
   * @returns syntax error at the original source coordinate
   */
  #createError(message: string, offset: number): ParseError {
    return new ParseError(
      'MDC_SYNTAX_INVALID',
      message,
      this.#context.locate(
        this.#view.mapOffset(offset),
        this.#view.mapOffset(offset),
      ).start,
    );
  }
}
