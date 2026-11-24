import { canInterruptParagraph, recognizeBlocks } from '../block';
import { countIndentation, removeIndent } from '../indentation';
import { sliceView, getExtent } from '../source';

import { trimSpace } from '../whitespace';

import type { BlockResult } from '../block';

import type { SourceLine, SourceContext, SourceView } from '../types';

const TAB_WIDTH = 4;

/**
 * groups quote-prefixed blocks and eligible lazy paragraph continuation
 * @param lines physical source lines
 * @param index current line index
 * @param context shared parsing and source context
 * @returns the resolved quote container or null
 */
export function readQuote(
  lines: readonly SourceLine[],
  index: number,
  context: SourceContext,
): BlockResult | null {
  const first = lines[index];

  if (!first || !/^ {0,3}>/.test(first.view.text)) {
    return null;
  }

  const body: SourceLine[] = [];
  let end = index;
  let isParagraph = false;

  for (let line = lines[end]; line; line = lines[end]) {
    const match = /^ {0,3}>/.exec(line.view.text);

    if (match) {
      const after = sliceView(line.view, match[0].length);
      const view = removeIndent(
        after,
        /^[ \t]/.test(after.text) ? 1 : 0,
        match[0].length,
      );
      body.push({ ...line, view });
      isParagraph = canContinueLazily(view, context);
      end++;
      continue;
    }

    if (
      !isParagraph ||
      !trimSpace(line.view.text) ||
      canInterruptParagraph(line.view, context)
    ) {
      break;
    }

    body.push({ ...line, isLazy: true });
    end++;
  }

  return {
    event: {
      type: 'blockquote',
      children: recognizeBlocks(body, context),
      view: getExtent(lines, index, end),
    },
    next: end,
  };
}

/**
 * determines whether the latest quote line admits lazy paragraph continuation
 * @param view quote body after its structural prefix
 * @param context shared parser profile
 * @returns whether a following unprefixed line may continue the quote
 */
function canContinueLazily(view: SourceView, context: SourceContext): boolean {
  if (!trimSpace(view.text)) {
    return false;
  }

  const prefix = /^(?: {0,3}>[ \t]?)+/.exec(view.text);

  if (
    !canInterruptParagraph(
      prefix ? sliceView(view, prefix[0].length) : view,
      context,
    ) &&
    countIndentation(view.text).columns < TAB_WIDTH
  ) {
    return true;
  }

  return /^ {0,3}(?:[-+*]|\d{1,9}[.)])[ \t]+/.test(view.text);
}
