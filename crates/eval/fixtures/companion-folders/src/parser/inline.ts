import { InlineScanner } from '#syntax/inline/scanner';

import {
  buildAnnotation,
  readInlineSelector,
  readInlineSuffix,
} from './inline/annotation';

import { AST_SINK } from './inline/sink';

import type { SourceContext, SourceView } from '#lexer/types';

import type { MdcxPhrasingContent } from '#types';

/**
 * resolves a complete inline region using original source coordinates
 * @param view mapped source region
 * @param context shared document context
 * @returns recursive phrasing nodes
 */
export function parseInline(
  view: SourceView,
  context: SourceContext,
): Array<MdcxPhrasingContent<string>> {
  context.budget.enter();
  const explicitKinds = new WeakMap<MdcxPhrasingContent<string>, string>();

  try {
    return new InlineScanner({
      view,
      context,
      sink: AST_SINK,
      definitions: context.definitions,
      isStrikethroughEnabled: context.options.profile !== 'commonmark',
      annotations:
        context.options.profile === 'commonmark'
          ? undefined
          : {
              open: readInlineSelector,
              /**
               * applies MDCX semantics after complete bracket recognition
               * @param input recognized annotation and source boundaries
               * @returns canonical node and complete suffix boundary
               */
              close(input) {
                const { selector, children, end, range } = input;
                const suffix = readInlineSuffix(view, end, context);
                const node = buildAnnotation({
                  selector,
                  suffix,
                  children,
                  context,
                  position: range.start,
                  explicitKinds,
                });
                const positioned = {
                  ...node,
                  range: {
                    start: range.start,
                    end: context.locate(
                      view.mapOffset(suffix.end),
                      view.mapOffset(suffix.end),
                    ).end,
                  },
                };
                const explicitKind =
                  selector.kind ??
                  (selector.type === undefined &&
                  children.length === 1 &&
                  children[0]
                    ? explicitKinds.get(children[0])
                    : undefined);

                if (explicitKind !== undefined) {
                  explicitKinds.set(positioned, explicitKind);
                }

                return {
                  node: positioned,
                  end: suffix.end,
                };
              },
            },
    }).scan();
  } finally {
    context.budget.leave();
  }
}
