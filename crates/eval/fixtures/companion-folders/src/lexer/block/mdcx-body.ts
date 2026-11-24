import { sliceView } from '../source';
import { trimSpace } from '../whitespace';

import type { BlockHeader } from '../controls';
import type { BlockSource, SourceLine } from '../types';

/** complete lexical opening before body ownership */
export interface BlockInput extends BlockSource {
  header: BlockHeader;
}

/**
 * consumes exactly the MDCX body prefix before Markdown recognizes opaque regions
 * @param input complete opening context
 * @param shouldTrimTrailing whether named-container separator lines are discarded
 * @returns the mapped body and its original exclusive line boundary
 */
export function readOwnedBody(
  input: BlockInput,
  shouldTrimTrailing = true,
): { body: SourceLine[]; end: number } {
  const { lines, index, header } = input;
  const body: SourceLine[] = [];
  const prefix = ' '.repeat(header.indent + 2);
  let end = index + 1;

  for (let current = lines[end]; current; current = lines[end]) {
    if (!trimSpace(current.view.text)) {
      body.push({
        ...current,
        view: sliceView(
          current.view,
          current.view.text.startsWith(prefix) ? prefix.length : 0,
        ),
      });
      end++;
      continue;
    }

    const closerPrefix = ' '.repeat(header.indent) + ';;';
    if (
      !current.view.text.startsWith(prefix) ||
      (current.view.text.startsWith(closerPrefix) &&
        (header.ref === undefined || current.view.text.includes(header.ref)))
    ) {
      break;
    }

    body.push({
      ...current,
      view: sliceView(current.view, header.indent + 2),
    });
    end++;
  }

  while (
    shouldTrimTrailing &&
    body.length &&
    !trimSpace(body.at(-1)?.view.text)
  ) {
    body.pop();
  }

  return { body, end };
}
