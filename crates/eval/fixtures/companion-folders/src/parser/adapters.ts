import { ParseError } from '#errors';
import { recognizeBlocks } from '#lexer/block';
import { getSourceLines } from '#lexer/source';
import { getChildren, isNode, isPhrasing, isFlow, validateTree } from '#tree';

import { createFragmentSource, omitSourceRanges } from './adapter-source';
import { buildAst } from './build';
import { parseInline } from './inline';

import type { BlockEvent, SourceContext, SourceView } from '#lexer/types';
import type {
  Adapter,
  AdapterRegistry,
  MdcxNode,
  MdcxRoot,
  MdcxFlowContent,
  MdcxPhrasingContent,
} from '#types';

/** semantic node and its original lexical body */
interface AdapterTarget {
  node: MdcxNode<string>;
  event: BlockEvent;
}

/**
 * applies codecs after native source identity and nesting validation
 * @param root validated native document
 * @param context shared parser context
 * @param events lexical event ownership
 * @returns adapted document retaining source envelopes
 */
export function applyAdapters(
  root: MdcxRoot<string>,
  context: SourceContext,
  events: ReadonlyMap<object, BlockEvent>,
): MdcxRoot<string> {
  if (context.options.profile === 'commonmark' || !context.options.adapters) {
    return root;
  }
  const adapters = context.options.adapters;
  const visit = (
    node: MdcxNode<string>,
    selection: { isRootChild?: boolean; bypassRecognition?: boolean } = {},
  ): MdcxNode<string> => {
    const event = events.get(node);
    if (
      event &&
      (!selection.bypassRecognition || event.selector !== undefined)
    ) {
      const adapted = recognizeAdapter({ node, event }, context, {
        isRootChild: selection.isRootChild,
        adapters,
      });
      if (adapted !== null) return adapted;
    }
    if (!('children' in node)) return node;
    const updated = {
      ...node,
      children: getChildren(node).map((child) =>
        visit(child, {
          bypassRecognition: bypassesBodyRecognition(event, adapters),
        }),
      ),
    };
    if (!isNode(updated)) {
      throw new ParseError(
        'MDC_AST_INVALID',
        'Adapter produced invalid child containment.',
      );
    }
    return updated;
  };

  return {
    ...root,
    children: root.children.map((child) => {
      const adapted = visit(child, { isRootChild: true });
      if (!isFlow(adapted)) {
        throw new ParseError(
          'MDC_AST_INVALID',
          'Adapter must retain flow content at document boundaries.',
        );
      }

      return adapted;
    }),
  };
}

/**
 * dispatches exact typed codecs or ordered untyped recognizers
 * @param target original semantic and lexical block
 * @param context shared parser context
 * @param selection registry and current-only bypass selection
 * @param selection.isRootChild whether current-only bypass applies
 * @param selection.adapters validated ordered semantic registry
 * @returns recognized block or native fallback
 */
function recognizeAdapter(
  target: AdapterTarget,
  context: SourceContext,
  selection: { isRootChild?: boolean; adapters: AdapterRegistry },
): MdcxNode<string> | null {
  const { event } = target;
  const candidates =
    event.selector === undefined
      ? Object.values(selection.adapters)
      : [selection.adapters[event.selector]];
  for (const adapter of candidates) {
    if (
      !adapter?.custom ||
      (selection.isRootChild && adapter === context.bypassAdapter)
    ) {
      continue;
    }
    if (
      (event.selector !== undefined || event.namespace !== undefined) &&
      adapter.kind !== undefined &&
      adapter.kind !== (target.node.kind ?? 'common')
    ) {
      continue;
    }
    const result = invokeParser(target, context, adapter);
    if (result !== null) {
      return retainEnvelope(target, result);
    }
  }

  return null;
}

/**
 * calls one codec with inherited recursive helpers and contextual failures
 * @param target semantic and lexical block
 * @param context inherited parsing controls
 * @param adapter selected body codec
 * @returns codec result or declined recognition
 */
function invokeParser(
  target: AdapterTarget,
  context: SourceContext,
  adapter: Adapter<MdcxNode<string>>,
): MdcxNode<string> | null {
  const { node, event } = target;
  const body = event.body ?? event.view;
  try {
    const result = adapter.custom?.parse(
      body.text,
      Object.freeze({
        type: event.selector,
        kind: node.kind ?? 'common',
        ref: node.ref,
        attributes: Object.freeze(structuredClone(node.attributes ?? {})),
        defaultParse: (source: string) =>
          parseFragment(source, context, { body, bypassAdapter: adapter }),
        parseBlocks: (source: string) =>
          parseFragment(source, context, { body }),
        parseInline: (source: string) =>
          parseInlineFragment(source, context, body),
      }),
    );
    if (result === null) {
      return null;
    }
    if (
      !isNode(result) ||
      (!isFlow(result) &&
        !['listItem', 'tableRow', 'tableCell'].includes(result.type))
    ) {
      throw new ParseError(
        'MDC_AST_INVALID',
        'Adapter parser must return a block node or null.',
      );
    }

    return result;
  } catch (cause) {
    const error = new ParseError(
      'MDC_AST_INVALID',
      `Adapter parser failed for ${event.selector ?? node.type}.`,
    );
    error.cause = cause;
    throw error;
  }
}

/**
 * keeps source annotation selectors authoritative over recognized content
 * @param target original semantic and lexical block
 * @param result recognized native node
 * @returns validated source-enveloped node
 */
function retainEnvelope(
  target: AdapterTarget,
  result: MdcxNode<string>,
): MdcxNode<string> {
  const { node, event } = target;
  const kind = node.kind;
  const adapted = {
    ...result,
    ...(event.selector === undefined && event.namespace === undefined
      ? {}
      : {
          kind,
          form: kind === undefined || kind === 'common' ? undefined : 'block',
        }),
    ...(event.selector === undefined ? {} : { type: node.type }),
    ...(node.ref === undefined ? {} : { ref: node.ref }),
    attributes: { ...node.attributes, ...result.attributes },
    range: node.range,
  };
  if (!isNode(adapted)) {
    throw new ParseError(
      'MDC_AST_INVALID',
      'Adapter returned an invalid semantic node.',
    );
  }

  return adapted;
}

/**
 * parses recursive bodies with the same grammar and shared nesting budget
 * @param source body source
 * @param parent current source context
 * @param selection lexical body and current recognizer bypass
 * @param selection.body original lexical mapping
 * @param selection.bypassAdapter recognizer skipped only for direct roots
 * @returns native flow body
 */
function parseFragment(
  source: string,
  parent: SourceContext,
  selection: { body: SourceView; bypassAdapter?: Adapter<MdcxNode<string>> },
): Array<MdcxFlowContent<string>> {
  parent.budget.enter();
  try {
    const { context, view, isOriginal } = createFragmentSource(
      source,
      parent,
      selection.body,
    );
    context.bypassAdapter = selection.bypassAdapter;
    const children = buildAst(
      recognizeBlocks(getSourceLines(view), context),
      context,
    ).children;

    return isOriginal ? children : children.map(omitSourceRanges);
  } finally {
    parent.budget.leave();
  }
}

/**
 * parses inline bodies through the native scanner and shared context
 * @param source phrasing source
 * @param parent current source context
 * @param body original lexical body mapping
 * @returns validated native phrasing
 */
function parseInlineFragment(
  source: string,
  parent: SourceContext,
  body: SourceView,
): Array<MdcxPhrasingContent<string>> {
  const { context, view, isOriginal } = createFragmentSource(
    source,
    parent,
    body,
  );
  const nodes = parseInline(view, context);
  validateTree({
    type: 'root',
    children: [{ type: 'paragraph', children: nodes }],
  });

  const validated = nodes.filter(isPhrasing);

  return isOriginal ? validated : validated.map(omitSourceRanges);
}

/**
 * protects explicitly selected native or registered bodies from unrelated codecs
 * @param event owning lexical block
 * @param adapters registered semantic codecs
 * @returns whether direct children retain the selected parent's native body
 */
function bypassesBodyRecognition(
  event: BlockEvent | undefined,
  adapters: AdapterRegistry,
): boolean {
  return (
    event?.selector !== undefined &&
    (!event.isCustom || adapters[event.selector] !== undefined)
  );
}
