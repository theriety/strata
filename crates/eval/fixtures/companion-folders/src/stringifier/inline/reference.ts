import { projectInline } from '#syntax/inline/projection';

import { isMarkdownShape } from '#tree';

import { hasMetadata, hasSelectedMetadata } from '../metadata';

import type { InlineSink } from '#syntax/inline/types';

import type { MdcxPhrasingContent } from '#types';

import type { WriterContext } from '../types';

/** read-only comparison output with no semantic child axis */
interface ContentSignature {
  signature: string;
  text?: string;
}

/** comparison source with its expected native content and resource policy */
interface ContentComparisonParams {
  source: string;
  children: ReadonlyArray<MdcxPhrasingContent<string>>;
  context: WriterContext;
  hasResources: boolean;
}

const signatureSink: InlineSink<ContentSignature> = {
  /**
   * preserves literal classification and decoded content
   * @param event recognized opaque or textual content
   * @returns scalar comparison output
   */
  createLiteral(event) {
    return getLiteralSignature(event.type, event.value);
  },
  /**
   * preserves formatting hierarchy
   * @param type resolved formatting role
   * @param children projected descendants
   * @returns scalar comparison output
   */
  createParent(type, children) {
    return getParentSignature(type, children);
  },
  /**
   * rejects resource syntax inside an inferred reference-link label
   * @returns a sentinel distinct from every native content signature
   */
  createLink() {
    // nested links deactivate their outer reference; images need forbidden brackets
    return { signature: 'resource' };
  },
  append: appendSignature,
};

/** treats resources as opaque boundaries whose own writer preserves their fields */
const formattingSink: InlineSink<ContentSignature> = {
  ...signatureSink,
  /**
   * retains resource boundaries around independently serialized label content
   * @param input recognized resource role
   * @returns opaque resource classification
   */
  createLink(input) {
    return getLiteralSignature(
      `${input.isImage ? 'image' : 'link'}${'identifier' in input.match ? 'Reference' : ''}`,
    );
  },
};

/**
 * checks native delimiter composition without reconstructing semantic nodes
 * @param source candidate native phrasing
 * @param children validated semantic phrasing
 * @param context document definition membership
 * @returns whether literal and formatting boundaries remain unchanged
 */
export function hasMatchingFormattingContent(
  source: string,
  children: ReadonlyArray<MdcxPhrasingContent<string>>,
  context: WriterContext,
): boolean {
  return hasMatchingContent({ source, children, context, hasResources: true });
}

/**
 * checks raw reference spelling against rich content without building another AST
 * @param source association spelling whose delimiters cannot be canonicalized
 * @param children validated semantic phrasing
 * @param context document definition membership
 * @returns whether native recognition preserves every visible content distinction
 */
export function hasMatchingReferenceContent(
  source: string,
  children: ReadonlyArray<MdcxPhrasingContent<string>>,
  context: WriterContext,
): boolean {
  return hasMatchingContent({ source, children, context, hasResources: false });
}

/**
 * compares native event signatures with the appropriate resource boundary policy
 * @param params candidate source, expected content and resource policy
 * @returns whether recognition preserves expected content
 */
function hasMatchingContent(params: ContentComparisonParams): boolean {
  const { source, children, context, hasResources } = params;
  const expected = getChildrenSignatures(children, hasResources, context);

  if (expected === undefined) {
    return false;
  }

  const actual = projectInline({
    source,
    definitions: context.definitions,
    isStrikethroughEnabled: context.profile === 'mdcx',
    sink: hasResources ? formattingSink : signatureSink,
  });

  return (
    JSON.stringify(actual.map((item) => item.signature)) ===
    JSON.stringify(expected.map((item) => item.signature))
  );
}

/**
 * projects supported native content directly to immutable comparison strings
 * @param node validated phrasing occurrence
 * @param hasResources whether resources are opaque comparison boundaries
 * @param context shared annotation emission policy
 * @returns comparison fields or absence when raw syntax cannot encode metadata
 */
function getNodeSignatures(
  node: MdcxPhrasingContent<string>,
  hasResources: boolean,
  context: WriterContext,
): ContentSignature[] | undefined {
  if (hasMetadata(node) && hasSelectedMetadata(node, context)) {
    return undefined;
  }

  if (!context.includes.includes('type') && !isMarkdownShape(node)) {
    return getChildrenSignatures(node.children, hasResources, context);
  }

  if (
    hasResources &&
    ['link', 'image', 'linkReference', 'imageReference'].includes(node.type)
  ) {
    return [getLiteralSignature(node.type)];
  }

  if ('value' in node) {
    return [
      getLiteralSignature(
        node.type,
        node.type === 'inlineCode' && context.isInTable
          ? node.value.replace(/\|/g, '\\|')
          : node.value,
      ),
    ];
  }

  if (node.type === 'break') {
    return [getLiteralSignature('break')];
  }

  if (!('children' in node)) {
    return undefined;
  }

  const children = getChildrenSignatures(node.children, hasResources, context);

  if (children === undefined) {
    return undefined;
  }

  return !context.includes.includes('type') && children.length === 0
    ? children
    : [getParentSignature(node.type, children)];
}

/**
 * coalesces visible comparison signatures across omitted application wrappers
 * @param nodes ordered semantic phrasing
 * @param hasResources whether resources are opaque comparison boundaries
 * @param context shared annotation emission policy
 * @returns visible signatures or absence for unrepresentable metadata
 */
function getChildrenSignatures(
  nodes: ReadonlyArray<MdcxPhrasingContent<string>>,
  hasResources: boolean,
  context: WriterContext,
): ContentSignature[] | undefined {
  const children: ContentSignature[] = [];

  for (const node of nodes) {
    const signatures = getNodeSignatures(node, hasResources, context);

    if (signatures === undefined) {
      return undefined;
    }

    for (const signature of signatures) {
      appendSignature(children, signature);
    }
  }

  return children;
}

/**
 * preserves formatting hierarchy in a scalar signature
 * @param type formatting classification
 * @param children recognized child projections
 * @returns collision-free comparison fields
 */
function getParentSignature(
  type: string,
  children: ContentSignature[],
): ContentSignature {
  return {
    // child signatures are already encoded; quoting them again expands with depth
    signature: `${JSON.stringify(type)}:[${children.map((child) => child.signature).join(',')}]`,
  };
}

/**
 * coalesces adjacent native text exactly as semantic normalization does
 * @param children ordered projection outputs
 * @param node next immutable projection
 */
function appendSignature(
  children: ContentSignature[],
  node: ContentSignature,
): void {
  const previous = children.at(-1);

  if (node.text === '') {
    return;
  }

  if (node.text !== undefined && previous?.text !== undefined) {
    children[children.length - 1] = getLiteralSignature(
      'text',
      previous.text + node.text,
    );
  } else {
    children.push(node);
  }
}

/**
 * distinguishes literal classes and decoded contents
 * @param type recognized literal classification
 * @param value decoded literal content
 * @returns collision-free comparison fields
 */
function getLiteralSignature(type: string, value?: string): ContentSignature {
  return {
    signature: JSON.stringify([type, value]),
    text: type === 'text' ? value : undefined,
  };
}
