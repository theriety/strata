import { canInterruptParagraph } from '../block';
import { countIndentation, removeIndent } from '../indentation';
import { getExtent, joinViews, sliceView } from '../source';

import { trimSpace, trimSpaceEnd } from '../whitespace';

import type { HeadingDepth } from '#types';

import type { BlockResult } from '../block';

import type { BlockSource, SourceLine, SourceView } from '../types';

const CODE_INDENT = 4;

/**
 * recognizes an ATX heading and its optional closing run
 * @param line current physical source line
 * @param index current source line index
 * @returns positioned heading or null
 */
export function readHeading(
  line: SourceLine,
  index: number,
): BlockResult | null {
  const text = line.view.text;
  const heading = /^ {0,3}(#{1,6})(?=[ \t]|$)/.exec(text);

  if (!heading) {
    return null;
  }

  let start = heading[0].length;

  while (text[start] === ' ' || text[start] === '\t') {
    start++;
  }

  let end = trimSpaceEnd(text).length;
  let hashes = end;

  while (text[hashes - 1] === '#') {
    hashes--;
  }

  if (
    hashes === start ||
    (hashes < end && /[ \t]/.test(text[hashes - 1] ?? ''))
  ) {
    end = trimSpaceEnd(text.slice(0, hashes)).length;
  }

  return {
    event: {
      type: 'heading',
      depth: heading[1]?.length as HeadingDepth,
      inline: sliceView(line.view, start, Math.max(start, end)),
      view: line.view,
    },
    next: index + 1,
  };
}

/**
 * recognizes indented literal code without losing remaining countIndentation
 * @param lines physical source lines
 * @param index current source line index
 * @param line current physical source line
 * @returns positioned code or null
 */
export function readIndented(
  lines: readonly SourceLine[],
  index: number,
  line: SourceLine,
): BlockResult | null {
  if (countIndentation(line.view.text).columns < CODE_INDENT) {
    return null;
  }

  {
    const body: SourceView[] = [];
    let end = index;

    for (let current = lines[end]; current; current = lines[end]) {
      if (
        trimSpace(current.view.text) &&
        countIndentation(current.view.text).columns < CODE_INDENT
      ) {
        break;
      }

      body.push(removeIndent(current.view, CODE_INDENT));
      end++;
    }

    while (body.length && !trimSpace(body.at(-1)?.text)) {
      body.pop();
    }

    return {
      event: {
        type: 'code',
        value: body.map((part) => part.text).join('\n'),
        view: getExtent(lines, index, end),
      },
      next: end,
    };
  }
}

/**
 * gathers a complete paragraph and resolves setext heading syntax
 * @param source existing current line and its shared source context
 * @returns positioned paragraph or heading
 */
export function readParagraph(source: BlockSource): BlockResult {
  const { lines, index, context, line } = source;
  const text = line.view.text;
  const body: SourceView[] = [
    sliceView(line.view, countIndentation(text).characters),
  ];
  let end = index + 1;
  let setext = 0;

  while (end < lines.length) {
    const current = lines[end];

    if (!current || !trimSpace(current.view.text)) {
      break;
    }

    if (!current.isLazy && /^ {0,3}(?:=+|-+)[ \t]*$/.test(current.view.text)) {
      setext = trimSpace(current.view.text).startsWith('=') ? 1 : 2;
      end++;
      break;
    }

    if (canInterruptParagraph(current.view, context)) {
      break;
    }

    body.push(
      sliceView(current.view, countIndentation(current.view.text).characters),
    );
    end++;
  }

  const inline = joinViews(body);

  return {
    event: {
      type: setext ? 'heading' : 'paragraph',
      depth: setext ? (setext as 1 | 2) : undefined,
      inline: sliceView(inline, 0, trimSpaceEnd(inline.text).length),
      view: getExtent(lines, index, end),
    },
    next: end,
  };
}
