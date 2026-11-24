import { BracketResolver } from './brackets';

import { readHtml } from './html';
import { isEscapable, readEntity } from './literals';
import { indexMatchingBackticks, inspectDelimiterRun } from './runs';
import {
  appendToken,
  buildChildren,
  resolveEmphasis,
  getTokenRange,
} from './tokens';

import type { SourceView } from '#lexer/types';

import type { Delimiter, InlineSequence, LiteralToken } from './tokens';

import type {
  InlineCoordinates,
  InlineEnvironment,
  InlineSink,
  InlineAnnotations,
  InlineLiteral,
} from './types';

const ASCII_SPACE = 32;

/** local scanner state; opaque spans and resolved brackets own their source */
export class InlineScanner<Node> {
  readonly #view: SourceView;
  readonly #context: InlineCoordinates;
  readonly #sink: InlineSink<Node>;
  readonly #annotations?: InlineAnnotations<Node>;
  readonly #definitions: Pick<ReadonlySet<string>, 'has'>;
  readonly #isStrikethroughEnabled: boolean;
  readonly #sequence: InlineSequence<Node> = {};
  readonly #closingOffsetByOpeningOffset: Map<number, number>;
  #cursor = 0;
  readonly #brackets: BracketResolver<Node>;

  /**
   * @param root0 source region and shared document context
   * @param root0.view mapped source region
   * @param root0.context shared document state
   * @param root0.sink selected output representation
   * @param root0.definitions available document associations
   * @param root0.annotations optional MDCX semantic adapter
   * @param root0.isStrikethroughEnabled whether deletion syntax is enabled
   */
  constructor({
    view,
    context,
    sink,
    definitions,
    annotations,
    isStrikethroughEnabled = false,
  }: InlineEnvironment<Node>) {
    this.#view = view;
    this.#context = context;
    this.#sink = sink;
    this.#definitions = definitions;
    this.#annotations = annotations;
    this.#isStrikethroughEnabled = isStrikethroughEnabled;
    this.#closingOffsetByOpeningOffset = indexMatchingBackticks(view.text);
    this.#brackets = new BracketResolver<Node>({
      view,
      context,
      sequence: this.#sequence,
      sink,
      definitions,
      annotations,
      isStrikethroughEnabled,
    });
  }

  /**
   * resolves the complete inline syntax region
   * @returns resolved semantic output
   */
  public scan(): Node[] {
    while (this.#cursor < this.#view.text.length) {
      this.#step();
    }

    this.#brackets.finish();
    resolveEmphasis(this.#sequence, undefined, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });

    return buildChildren(this.#sequence.first, undefined, {
      view: this.#view,
      context: this.#context,
      sink: this.#sink,
      definitions: this.#definitions,
    });
  }

  /** consumes source or changes the active syntax state */
  #step(): void {
    const character = this.#view.text[this.#cursor];

    switch (character) {
      case '\\':
        this.#escape();

        return;
      case '`':
        this.#readCode();

        return;
      case '[':
        this.#cursor = this.#brackets.open(this.#cursor, false);

        return;
      case ']':
        this.#cursor = this.#brackets.close(this.#cursor);

        return;
      case '*':
      case '_':
        this.#readDelimiter(character);

        return;
      case '\n':
        this.#readNewline();

        return;
      default:
        if (!this.#readOptionalSyntax(character)) {
          this.#readPlain();
        }
    }
  }

  /**
   * recognizes optional and multi-character opening controls
   * @param character current source character
   * @returns whether syntax consumed source
   */
  #readOptionalSyntax(character: string | undefined): boolean {
    if (character === '&') {
      return this.#readEntity();
    }

    if (character === '<') {
      return this.#readAngle();
    }

    if (character === '!' && this.#view.text[this.#cursor + 1] === '[') {
      this.#cursor = this.#brackets.open(this.#cursor, true);

      return true;
    }

    if (character === ':' && this.#annotations) {
      const end = this.#brackets.openAnnotation(
        this.#cursor,
        this.#annotations,
      );

      if (end === undefined) {
        return false;
      }

      this.#cursor = end;

      return true;
    }

    if (character === '~' && this.#isStrikethroughEnabled) {
      this.#readDelimiter(character);

      return true;
    }

    return false;
  }

  /** consumes a literal run until the next active marker */
  #readPlain(): void {
    const start = this.#cursor++;

    while (
      this.#cursor < this.#view.text.length &&
      !/[\\`&<:![\]*_~\n]/.test(this.#view.text.charAt(this.#cursor))
    ) {
      this.#cursor++;
    }

    const value = this.#view.text.slice(start, this.#cursor);
    this.#appendText(value, start, this.#cursor);
  }

  /** interprets a backslash only in an active inline context */
  #escape(): void {
    const start = this.#cursor;
    const next = this.#view.text[start + 1] ?? '';

    if (next === '\n') {
      this.#cursor += 2;
      this.#appendNode({ type: 'break' }, start, this.#cursor);

      while (/[ \t]/.test(this.#view.text.charAt(this.#cursor))) {
        this.#cursor++;
      }
    } else if (isEscapable(next)) {
      this.#cursor += 2;
      this.#appendText(next, start, this.#cursor);
    } else {
      this.#cursor++;
      this.#appendText('\\', start, this.#cursor);
    }
  }

  /** resolves a soft or hard line ending */
  #readNewline(): void {
    const start = this.#cursor++;
    const previous = this.#sequence.last;
    let spaces = 0;
    let whitespace = 0;
    const value = previous?.value ?? '';

    while (this.#view.text[start - spaces - 1] === ' ') {
      spaces++;
    }

    while (/[ \t]/.test(this.#view.text.charAt(start - whitespace - 1))) {
      whitespace++;
    }

    if (previous?.value) {
      previous.value = value.slice(0, value.length - whitespace);
      previous.end -= whitespace;
    }

    if (spaces >= 2) {
      this.#appendNode({ type: 'break' }, start - spaces, this.#cursor);
    } else {
      this.#appendText('\n', start, this.#cursor);
    }

    while (/[ \t]/.test(this.#view.text.charAt(this.#cursor))) {
      this.#cursor++;
    }
  }

  /** consumes an opaque matching backtick run */
  #readCode(): void {
    const start = this.#cursor;

    while (this.#view.text[this.#cursor] === '`') {
      this.#cursor++;
    }

    const length = this.#cursor - start;
    const end = this.#closingOffsetByOpeningOffset.get(start);

    if (end === undefined) {
      this.#appendText('`'.repeat(length), start, this.#cursor);

      return;
    }

    let value = this.#view.text.slice(this.#cursor, end).replace(/\n/g, ' ');

    if (value.startsWith(' ') && value.endsWith(' ') && /[^ ]/.test(value)) {
      value = value.slice(1, -1);
    }

    this.#cursor = end + length;
    this.#appendNode({ type: 'inlineCode', value }, start, this.#cursor);
  }

  /**
   * decodes a recognized character reference
   * @returns whether a character reference was consumed
   */
  #readEntity(): boolean {
    const match = readEntity(this.#view.text, this.#cursor);

    if (!match) {
      return false;
    }

    this.#appendText(match.value, this.#cursor, match.end);
    this.#cursor = match.end;

    return true;
  }

  /**
   * recognizes an autolink or opaque inline HTML
   * @returns whether an autolink or HTML fragment was consumed
   */
  #readAngle(): boolean {
    const source = this.#view.text.slice(this.#cursor);
    const uri = /^<([A-Za-z][A-Za-z0-9+.-]{1,31}:[^<>]*)>/.exec(source);
    const email =
      /^<([A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*)>/.exec(
        source,
      );
    const validUri =
      uri?.[1] &&
      [...uri[1]].every((character) => character.charCodeAt(0) > ASCII_SPACE)
        ? uri
        : undefined;
    const match = validUri ?? email;

    if (match?.[1]) {
      const value = match[1];
      const start = this.#cursor;
      this.#cursor += match[0].length;
      const range = getTokenRange(
        { start, end: this.#cursor },
        this.#view,
        this.#context,
      );
      const child = this.#sink.createLiteral(
        { type: 'text', value },
        this.#context.locate(
          this.#view.mapOffset(start + 1),
          this.#view.mapOffset(this.#cursor - 1),
        ),
      );
      const node = this.#sink.createLink({
        match: { url: validUri ? value : `mailto:${value}`, end: this.#cursor },
        isImage: false,
        children: [child],
        range,
      });
      appendToken(this.#sequence, { node, start, end: this.#cursor });
      this.#brackets.deactivateLinks();

      return true;
    }

    const html = readHtml(source);

    if (!html) {
      return false;
    }

    const start = this.#cursor;
    this.#cursor += html.length;
    this.#appendNode({ type: 'html', value: html }, start, this.#cursor);

    return true;
  }

  /**
   * records delimiter-run eligibility
   * @param marker delimiter marker character
   */
  #readDelimiter(marker: string): void {
    const start = this.#cursor;
    const { end, length, ...eligibility } = inspectDelimiterRun({
      source: this.#view.text,
      start,
      marker,
    });
    this.#cursor = end;
    const token = this.#appendText(marker.repeat(length), start, this.#cursor);

    if (marker === '~' && length > 2) {
      return;
    }

    const delimiter: Delimiter<Node> = {
      token,
      marker,
      length,
      ...eligibility,
      previous: this.#sequence.delimiter,
    };

    if (this.#sequence.delimiter) {
      this.#sequence.delimiter.next = delimiter;
    }

    this.#sequence.delimiter = delimiter;
  }

  /**
   * appends a positioned semantic leaf event
   * @param event resolved literal event
   * @param start inclusive source boundary
   * @param end exclusive source boundary
   */
  #appendNode(event: InlineLiteral, start: number, end: number): void {
    const range = getTokenRange({ start, end }, this.#view, this.#context);
    appendToken(this.#sequence, {
      start,
      end,
      node: this.#sink.createLiteral(event, range),
    });
  }

  /**
   * appends literal source text
   * @param value literal value
   * @param start inclusive source boundary
   * @param end exclusive source boundary
   * @returns literal token appended to the active sequence
   */
  #appendText(value: string, start: number, end: number): LiteralToken<Node> {
    return appendToken(this.#sequence, { value, start, end });
  }
}
