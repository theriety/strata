import { ParseError } from '#errors';

import { findFlowMappingEnd } from '#lexer/mapping';

import { isMarkdownPhrasingShape, resolveNodeKind } from '#tree';

import { parseAnnotationMapping } from '../annotation';

import type { SourceContext, SourceView } from '#lexer/types';

import type { InlineSelector } from '#syntax/inline/types';

import type { Attributes, MdcxPhrasingContent, SourcePosition } from '#types';

/** semantic input to a recognized annotation builder */
export interface BuildAnnotationParams {
  selector: InlineSelector;
  suffix: InlineSuffix;
  children: Array<MdcxPhrasingContent<string>>;
  context: SourceContext;
  position: SourcePosition;
  explicitKinds: WeakMap<MdcxPhrasingContent<string>, string>;
}

/** parsed suffix without structural selectors in its attributes */
export interface InlineSuffix {
  end: number;
  attributes?: Attributes;
  ref?: string;
}

/** source capabilities used by annotation diagnostics */
interface InlineEnvironment {
  view: SourceView;
  context: SourceContext;
}

/**
 * recognizes adjacent inline selector components
 * @param source complete inline source
 * @param start single-colon boundary
 * @returns selector only when followed by its required content bracket
 */
export function readInlineSelector(
  source: string,
  start: number,
): InlineSelector | undefined {
  const match =
    /^:(?:([A-Za-z][A-Za-z0-9_-]*)(?::([A-Za-z][A-Za-z0-9_-]*)?)?)?\[/.exec(
      source.slice(start),
    );

  if (!match) {
    return undefined;
  }

  const isQualified = match[0].slice(1).includes(':');

  return {
    type: isQualified ? match[2] : match[1],
    kind: isQualified ? match[1] : undefined,
    bracket: start + match[0].length - 1,
  };
}

/**
 * scans a single-line balanced mapping and adjacent reference suffix
 * @param view mapped inline source
 * @param start position after the content bracket
 * @param context document diagnostics and resolution options
 * @returns validated semantic annotation suffix
 */
export function readInlineSuffix(
  view: SourceView,
  start: number,
  context: SourceContext,
): InlineSuffix {
  let cursor = start;
  let attributes: Attributes | undefined;

  if (view.text[cursor] === '{') {
    const mappingStart = cursor;
    const end = findFlowMappingEnd({
      source: view.text,
      startIndex: cursor,
      options: {
        position: context.locate(view.mapOffset(cursor), view.mapOffset(cursor))
          .start,
      },
    });

    if (end === null) {
      throw createSyntaxError(
        'Unterminated inline attribute mapping.',
        cursor,
        {
          view,
          context,
        },
      );
    }

    attributes = parseAnnotationMapping({
      mapping: end,
      getPosition: (offset) =>
        context.locate(
          view.mapOffset(mappingStart + offset),
          view.mapOffset(mappingStart + offset),
        ).start,
      context: {
        name: 'inline annotation',
        position: context.locate(view.mapOffset(cursor), view.mapOffset(cursor))
          .start,
      },
    });
    cursor = end.end;
  }

  let ref: string | undefined;

  if (view.text[cursor] === '#') {
    const match = /^#(\w[A-Za-z0-9_-]*)/.exec(view.text.slice(cursor));

    if (!match) {
      throw createSyntaxError(
        'An inline reference requires an identifier.',
        cursor,
        {
          view,
          context,
        },
      );
    }

    ref = match[1];
    cursor += match[0].length;
  }

  return { attributes, ref, end: cursor };
}

/**
 * constructs a classified annotation after syntax resolution
 * @param params resolved selector, suffix, children, and parser context
 * @returns one canonical annotated target
 * @throws {ParseError} when the committed annotation has no permitted inline target
 */
export function buildAnnotation(
  params: BuildAnnotationParams,
): MdcxPhrasingContent<string> {
  const { selector, suffix, children } = params;

  if (selector.type) {
    const kind = resolveAnnotationKind(params, selector.type, 'custom');

    if (kind === 'common') {
      throw new ParseError(
        'MDC_SYNTAX_INVALID',
        'Native inline annotations infer their type; use :common:[content].',
      );
    }

    return {
      type: selector.type,
      kind,
      form: 'inline',
      attributes: suffix.attributes,
      ref: suffix.ref,
      children,
    };
  }

  const [target] = children;

  if (children.length !== 1 || !target) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'An anonymous inline annotation must contain exactly one complete inline target.',
    );
  }

  const explicitKind = params.explicitKinds.get(target);

  if (
    selector.kind !== undefined &&
    explicitKind !== undefined &&
    selector.kind !== explicitKind
  ) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'An inline annotation cannot overwrite an explicit target namespace.',
      params.position,
    );
  }

  return annotateTarget(
    target,
    suffix,
    selector.kind !== undefined
      ? resolveAnnotationKind(params, target.type, 'common')
      : (target.kind ?? resolveAnnotationKind(params, target.type, 'common')),
  );
}

/**
 * resolves the recognized classification after the target category is established
 * @param params semantic selector and attributes
 * @param type complete named or inferred classification
 * @param fallback namespace associated with that target category
 * @returns final validated namespace
 */
function resolveAnnotationKind(
  params: BuildAnnotationParams,
  type: string,
  fallback: 'common' | 'custom',
): string {
  const { selector, suffix, context } = params;

  // HTML remains opaque native phrasing, but its semantic annotations require
  // a block boundary; a differently named custom wrapper may still contain it
  if (type === 'html') {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'HTML annotations require block syntax; inline HTML annotation targets are not supported.',
      params.position,
    );
  }

  return resolveNodeKind(
    {
      type,
      form: 'inline',
      ref: suffix.ref,
      attributes: suffix.attributes ?? {},
    },
    context.options,
    {
      explicitKind: selector.kind,
      fallback,
    },
  );
}

/**
 * applies metadata directly to a complete inferred native shape
 * @param target recognized Markdown target
 * @param suffix validated attribute and identity suffix
 * @param kind resolved namespace
 * @returns classified native-shaped target
 */
function annotateTarget(
  target: MdcxPhrasingContent<string>,
  suffix: InlineSuffix,
  kind: string,
): MdcxPhrasingContent<string> {
  if (!isMarkdownPhrasingShape(target)) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'Anonymous annotations require a complete Markdown inline target.',
    );
  }

  if (target.ref && suffix.ref) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'An inline target cannot declare two references.',
    );
  }

  const outerAttributes =
    suffix.attributes && Object.keys(suffix.attributes).length > 0
      ? suffix.attributes
      : undefined;

  if (
    outerAttributes &&
    target.attributes &&
    Object.keys(target.attributes).length > 0
  ) {
    throw new ParseError(
      'MDC_SYNTAX_INVALID',
      'An inline annotation cannot overwrite attributes already declared on its target.',
    );
  }

  const metadata = {
    attributes: outerAttributes ?? target.attributes,
    ref: suffix.ref ?? target.ref,
  };

  if (kind === 'common') {
    const { kind: _previousKind, form: _previousForm, ...native } = target;

    return { ...native, ...metadata, kind: 'common' };
  }

  return { ...target, ...metadata, kind, form: 'inline' };
}

/**
 * creates a source-mapped annotation diagnostic
 * @param message actionable diagnostic description
 * @param offset source character boundary
 * @param environment source mapping and document context
 * @returns positioned parser error
 */
function createSyntaxError(
  message: string,
  offset: number,
  environment: InlineEnvironment,
): ParseError {
  const { view, context } = environment;

  return new ParseError(
    'MDC_SYNTAX_INVALID',
    message,
    context.locate(view.mapOffset(offset), view.mapOffset(offset)).start,
  );
}
