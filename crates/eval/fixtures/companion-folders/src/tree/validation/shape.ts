import type { NodeRecord } from './record';

type ShapeCheck = (value: Record<string, unknown>) => boolean;

export const FLOW = new Set([
  'paragraph',
  'heading',
  'blockquote',
  'list',
  'code',
  'math',
  'html',
  'thematicBreak',
  'definition',
  'table',
]);

export const PHRASING = new Set([
  'text',
  'inlineCode',
  'inlineMath',
  'html',
  'break',
  'image',
  'imageReference',
  'emphasis',
  'strong',
  'delete',
  'link',
  'linkReference',
]);

export const FIELDS = {
  root: ['children'],
  paragraph: ['children'],
  heading: ['depth', 'children'],
  blockquote: ['children'],
  list: ['ordered', 'start', 'spread', 'children'],
  listItem: ['spread', 'checked', 'children'],
  table: ['align', 'children'],
  tableRow: ['children'],
  tableCell: ['children'],
  text: ['value'],
  inlineCode: ['value'],
  inlineMath: ['value'],
  html: ['value'],
  code: ['value', 'lang', 'meta'],
  math: ['value', 'meta'],
  break: [],
  thematicBreak: [],
  emphasis: ['children'],
  strong: ['children'],
  delete: ['children'],
  link: ['url', 'title', 'children'],
  image: ['url', 'title', 'alt'],
  definition: ['identifier', 'label', 'url', 'title'],
  linkReference: ['identifier', 'label', 'referenceType', 'children'],
  imageReference: ['identifier', 'label', 'referenceType', 'alt'],
} as const;

const MAX_LIST_START = 999999999;

const MAX_HEADING_DEPTH = 6;

const SPECIAL_CHECKS: Readonly<Record<string, ShapeCheck>> = {
  heading: (value) =>
    typeof value.depth === 'number' &&
    Number.isInteger(value.depth) &&
    value.depth >= 1 &&
    value.depth <= MAX_HEADING_DEPTH,
  inlineCode: (value) =>
    typeof value.value === 'string' &&
    value.value !== '' &&
    !/[\r\n]/.test(value.value),
  code: (value) =>
    value.meta === undefined ||
    value.meta === null ||
    (typeof value.lang === 'string' && value.lang.length > 0),
  list: isValidList,
  table: isValidTable,
};

/**
 * checks required and optional native semantic fields
 * @param value candidate native shape
 * @returns whether field contracts hold
 */
export function hasValidFields(value: NodeRecord): boolean {
  if (!hasNativeType(value.type)) {
    return false;
  }

  const fields: readonly string[] = FIELDS[value.type];

  if (fields.includes('children') && !Array.isArray(value.children)) {
    return false;
  }

  if (fields.includes('value') && typeof value.value !== 'string') {
    return false;
  }

  if (fields.includes('url') && typeof value.url !== 'string') {
    return false;
  }

  if (
    fields.includes('identifier') &&
    (typeof value.identifier !== 'string' || value.identifier === '')
  ) {
    return false;
  }

  if (
    fields.includes('referenceType') &&
    (typeof value.referenceType !== 'string' ||
      !['full', 'collapsed', 'shortcut'].includes(value.referenceType))
  ) {
    return false;
  }

  return (
    hasValidOptionalFields(value, fields) &&
    (SPECIAL_CHECKS[value.type]?.(value) ?? true)
  );
}

/**
 * narrows a construct discriminator to supported native fields
 * @param type construct discriminator
 * @returns whether a native structural specification exists
 */
export function hasNativeType(type: string): type is keyof typeof FIELDS {
  return Object.hasOwn(FIELDS, type);
}

/**
 * validates optional primitive fields without coercion
 * @param value candidate shape
 * @param fields supported fields for this shape
 * @returns whether optional primitives have their specified types
 */
function hasValidOptionalFields(
  value: Record<string, unknown>,
  fields: readonly string[],
): boolean {
  for (const key of ['title', 'label', 'alt', 'lang', 'meta']) {
    if (fields.includes(key) && !isOptionalPrimitive(value[key], 'string')) {
      return false;
    }
  }

  for (const key of ['ordered', 'spread', 'checked']) {
    if (fields.includes(key) && !isOptionalPrimitive(value[key], 'boolean')) {
      return false;
    }
  }

  return true;
}

/**
 * permits null and absence for optional mdast primitives
 * @param value optional field value
 * @param primitive expected primitive type
 * @returns whether the convention is satisfied
 */
function isOptionalPrimitive(value: unknown, primitive: string): boolean {
  return value === undefined || value === null || typeof value === primitive;
}

/**
 * checks list starting values against marker representability
 * @param value candidate list
 * @returns whether the start can be serialized
 */
function isValidList(value: Record<string, unknown>): boolean {
  if (value.start === undefined || value.start === null) {
    return true;
  }

  if (value.ordered !== true) {
    return false;
  }

  return (
    typeof value.start === 'number' &&
    Number.isSafeInteger(value.start) &&
    value.start >= 0 &&
    value.start <= MAX_LIST_START
  );
}

/**
 * checks GFM table header and column alignment
 * @param value candidate table
 * @returns whether a header and valid alignments exist
 */
function isValidTable(value: Record<string, unknown>): boolean {
  if (!Array.isArray(value.children) || value.children.length === 0) {
    return false;
  }

  if (value.align === undefined || value.align === null) {
    return true;
  }

  return (
    Array.isArray(value.align) &&
    value.align.every(
      (item: unknown) =>
        item === null ||
        item === 'left' ||
        item === 'center' ||
        item === 'right',
    )
  );
}
