import { linkNode } from './link';

import type { InlineSink } from '#syntax/inline/types';

import type { MdcxPhrasingContent, SourceRange } from '#types';

/** every parser-produced inline occurrence retains its original source extent */
export type PositionedPhrasing = MdcxPhrasingContent<string> & {
  range: SourceRange;
};

/** constructs the canonical semantic nodes from resolved inline events */
export const AST_SINK: InlineSink<PositionedPhrasing> = {
  /**
   * constructs a native literal without changing its value
   * @param event resolved literal event
   * @param range original source span
   * @returns canonical literal node
   */
  createLiteral(event, range) {
    switch (event.type) {
      case 'break':
        return { type: 'break', range };
      case 'text':
        return { type: 'text', value: event.value, range };
      case 'inlineCode':
        return { type: 'inlineCode', value: event.value, range };
      default:
        return { type: 'html', value: event.value, range };
    }
  },
  /**
   * constructs recursive formatting
   * @param type resolved formatting role
   * @param children canonical phrasing descendants
   * @param range original source span
   * @returns canonical formatting node
   */
  createParent(type, children, range) {
    switch (type) {
      case 'strong':
        return { type: 'strong', children, range };
      case 'emphasis':
        return { type: 'emphasis', children, range };
      default:
        return { type: 'delete', children, range };
    }
  },
  /**
   * constructs a link or textual image alternative
   * @param input recognized bracket association
   * @param input.match resolved destination or reference
   * @param input.isImage whether this is an image
   * @param input.children canonical label descendants
   * @param input.range original source span
   * @returns canonical link or image node
   */
  createLink(input) {
    const { match, isImage, children, range } = input;

    return { ...linkNode(match, isImage, children), range };
  },
  /**
   * merges adjacent unannotated native text while retaining annotation boundaries
   * @param children canonical sequence being constructed
   * @param node next resolved node
   * @param isRaw whether the node came from literal source events
   */
  append(children, node, isRaw) {
    const previous = children.at(-1);

    if (
      isRaw &&
      node.type === 'text' &&
      'value' in node &&
      previous?.type === 'text' &&
      'value' in previous &&
      previous.kind === undefined &&
      !previous.ref &&
      !previous.attributes
    ) {
      previous.value += node.value;
      previous.range.end = node.range.end;
    } else {
      children.push(node);
    }
  },
};
