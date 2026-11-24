import { checkMetadata } from './validation/metadata';

import { fail, isRecord, checkRecord } from './validation/record';

import {
  FLOW,
  PHRASING,
  FIELDS,
  hasValidFields,
  hasNativeType,
} from './validation/shape';

import type {
  RuntimeNamespace,
  ApplicationShape,
  NativeFlowContent,
  NativePhrasingContent,
  ListItem,
  TableRow,
  TableCell,
  MdcxNode,
  MdcxRoot,
  MdcxFlowContent,
  MdcxPhrasingContent,
  NativeNode,
} from '#types';

import type { NodeRecord } from './validation/record';

/** validated Markdown-shaped nodes across namespaces */
export type MarkdownShape =
  | NativeNode<string>
  | ApplicationShape<
      | NativeFlowContent<string>
      | ListItem<string>
      | TableRow<string>
      | TableCell<string>,
      string
    >
  | ApplicationShape<NativePhrasingContent<string>, string, 'inline'>;

type Category = 'document' | 'flow' | 'phrasing' | 'item' | 'row' | 'cell';

interface PlacementParams {
  category: string;
  isNative: boolean;
  isShape: boolean;
}

interface ChildrenParams {
  isShape: boolean;
  isLinked: boolean;
  state: TreeState;
}

interface TreeState {
  ancestors: Set<object>;
  identities: Set<string>;
  depth: number;
  isInsideLink: boolean;
  isInsideCell: boolean;
}

const BASE = new Set([
  'type',
  'kind',
  'form',
  'ref',
  'attributes',
  'range',
  'data',
]);

const MAX_DEPTH = 128;

const CATEGORY_TYPES: Readonly<Record<string, ReadonlySet<string>>> = {
  document: new Set(['root']),
  flow: FLOW,
  phrasing: PHRASING,
  item: new Set(['listItem']),
  row: new Set(['tableRow']),
  cell: new Set(['tableCell']),
};

const CHILD_CATEGORIES: Readonly<Record<string, Category>> = {
  list: 'item',
  table: 'row',
  tableRow: 'cell',
  paragraph: 'phrasing',
  heading: 'phrasing',
  emphasis: 'phrasing',
  strong: 'phrasing',
  delete: 'phrasing',
  link: 'phrasing',
  linkReference: 'phrasing',
  tableCell: 'phrasing',
};

/**
 * validates external document trees before consuming them
 * @param value external semantic tree
 */
export function validateTree(
  value: unknown,
): asserts value is MdcxRoot<RuntimeNamespace> {
  checkNode(value, 'document', createInitialState());
}

/**
 * checks a complete supported node
 * @param value external node
 * @returns whether the node is valid in any supported context
 */
export function isNode(value: unknown): value is MdcxNode<RuntimeNamespace> {
  return Object.keys(CATEGORY_TYPES).some((category) =>
    isValid(value, category),
  );
}

/**
 * checks a complete document shape
 * @param value external document
 * @returns whether the document is valid
 */
export function isRoot(value: unknown): value is MdcxRoot<RuntimeNamespace> {
  return isValid(value, 'document');
}

/**
 * checks a complete phrasing subtree
 * @param value external phrasing node
 * @returns whether the phrasing subtree is valid
 */
export function isPhrasing(
  value: unknown,
): value is MdcxPhrasingContent<RuntimeNamespace> {
  return isValid(value, 'phrasing');
}

/**
 * checks a complete flow subtree
 * @param value external flow node
 * @returns whether the flow subtree is valid
 */
export function isFlow(
  value: unknown,
): value is MdcxFlowContent<RuntimeNamespace> {
  return isValid(value, 'flow');
}

/**
 * checks Markdown fields without discarding application identity
 * @param value canonical node
 * @returns whether complete Markdown contracts hold
 */
export function isMarkdownShape(
  value: MdcxNode<string>,
): value is MarkdownShape {
  if (value.form === 'document' && value.kind !== 'common') {
    return false;
  }

  if (
    value.kind !== undefined &&
    value.kind !== 'common' &&
    !isValidForm(value.type, value.form)
  ) {
    return false;
  }

  if (!hasNativeType(value.type)) {
    return false;
  }

  return isValid(
    { ...value, kind: undefined, form: undefined, ref: undefined },
    getCategory(value.type),
  );
}

/**
 * checks an external shape without exposing diagnostic exceptions
 * @param value external node
 * @param category required containment
 * @returns whether validation succeeds
 */
function isValid(value: unknown, category: string): boolean {
  try {
    checkNode(value, category, createInitialState());

    return true;
  } catch {
    return false;
  }
}

/**
 * creates independent validation ancestry and identity sets
 * @returns fresh validation state
 */
function createInitialState(): TreeState {
  return {
    ancestors: new Set(),
    identities: new Set(),
    depth: 0,
    isInsideLink: false,
    isInsideCell: false,
  };
}

/**
 * selects the preferred standalone context of a Markdown node
 * @param type Markdown discriminator
 * @returns node category
 */
function getCategory(type: string): Category {
  if (type === 'root') {
    return 'document';
  }

  if (type === 'listItem') {
    return 'item';
  }

  if (type === 'tableRow') {
    return 'row';
  }

  if (type === 'tableCell') {
    return 'cell';
  }

  return FLOW.has(type) ? 'flow' : 'phrasing';
}

/**
 * validates one occurrence and then its children using shared ancestry
 * @param value external occurrence
 * @param category required child category
 * @param state shared identity and ancestry state
 */
function checkNode(value: unknown, category: string, state: TreeState): void {
  checkRecord(value);

  if (state.depth > MAX_DEPTH) {
    fail(`Tree nesting exceeds ${MAX_DEPTH}.`);
  }

  if (state.ancestors.has(value)) {
    fail('Tree contains a cycle.');
  }

  const isNative = checkMetadata(value, state.identities) === 'common';
  const isShape = isNative || isValidShape(value);

  if (isNative && !hasValidFields(value)) {
    fail(`Invalid native ${value.type} fields.`);
  }

  checkPlacement(value, { category, isNative, isShape });
  checkFields(value, isShape);

  if (state.isInsideCell && isShape && value.type === 'break') {
    fail('GFM cells cannot contain hard line breaks.');
  }

  const isLinked =
    isShape && (value.type === 'link' || value.type === 'linkReference');

  if (state.isInsideLink && isLinked) {
    fail('Links cannot contain link descendants.');
  }

  checkChildren(value, { isShape, isLinked, state });
}

/**
 * visits descendants without resetting depth or identity budgets
 * @param value current parent
 * @param params shared context and shape classification
 * @param params.isShape whether native fields apply
 * @param params.isLinked whether the current node is a link
 * @param params.state document validation state
 */
function checkChildren(
  value: NodeRecord,
  { isShape, isLinked, state }: ChildrenParams,
): void {
  if (value.children === undefined) {
    return;
  }

  if (!Array.isArray(value.children)) {
    fail('Children must be an array.');
  }

  const category = isShape
    ? (CHILD_CATEGORIES[value.type] ?? 'flow')
    : value.form === 'inline'
      ? 'phrasing'
      : 'flow';
  state.ancestors.add(value);

  for (const child of value.children) {
    checkNode(child, category, {
      ...state,
      depth: state.depth + 1,
      isInsideLink: state.isInsideLink || isLinked,
      isInsideCell:
        state.isInsideCell || (isShape && value.type === 'tableCell'),
    });
  }

  state.ancestors.delete(value);
}

/**
 * checks whether an application node supplies a native body shape
 * @param value node record
 * @returns whether its fields and immediate child categories match
 */
function isValidShape(value: NodeRecord): boolean {
  if (
    !isValidForm(value.type, value.form) ||
    !hasNativeType(value.type) ||
    !hasValidFields(value)
  ) {
    return false;
  }

  const allowed: readonly string[] = FIELDS[value.type];

  if (
    Object.keys(value).some((key) => !BASE.has(key) && !allowed.includes(key))
  ) {
    return false;
  }

  const category = CHILD_CATEGORIES[value.type];

  if (!category || !Array.isArray(value.children)) {
    return true;
  }

  return value.children.every((child: unknown) => isMember(child, category));
}

/**
 * checks immediate containment without restarting recursive budgets
 * @param value candidate child
 * @param category required category
 * @returns whether child identity allows that category
 */
function isMember(value: unknown, category: Category): boolean {
  if (!isRecord(value) || typeof value.type !== 'string') {
    return false;
  }

  if (CATEGORY_TYPES[category]?.has(value.type)) {
    return true;
  }

  if (value.kind === undefined || value.kind === 'common') {
    return false;
  }

  return category === 'phrasing' && value.form === 'inline';
}

/**
 * checks root and structural containment without inferring ownership
 * @param value node record
 * @param params placement constraints
 * @param params.category containing category
 * @param params.isNative whether namespace is common
 * @param params.isShape whether native semantic fields apply
 */
function checkPlacement(
  value: NodeRecord,
  { category, isNative, isShape }: PlacementParams,
): void {
  if (category === 'document') {
    if (isNative ? value.type !== 'root' : value.form !== 'document') {
      fail('Expected a document root.');
    }

    return;
  }

  if (value.form === 'document' || (isNative && value.type === 'root')) {
    fail('Document roots cannot occur among descendants.');
  }

  if (isShape) {
    checkShapePlacement(value, category);

    return;
  }

  if (
    (category === 'flow' && value.form === 'block') ||
    (category === 'phrasing' && value.form === 'inline')
  ) {
    return;
  }

  fail(`Invalid application node in ${category}.`);
}

/**
 * validates native shape placement and corresponding application form
 * @param value node record
 * @param category required category
 */
function checkShapePlacement(value: NodeRecord, category: string): void {
  // HTML belongs to both native categories; application form still owns placement
  if (
    value.form !== undefined &&
    ((category === 'phrasing' && value.form !== 'inline') ||
      (category === 'flow' && value.form !== 'block'))
  ) {
    fail(`Application form is invalid in ${category} content.`);
  }

  if (!CATEGORY_TYPES[category]?.has(value.type)) {
    fail(`Invalid ${value.type} in ${category} content.`);
  }
}

/**
 * rejects unsupported top-level fields
 * @param value node record
 * @param isShape whether native fields apply
 */
function checkFields(value: NodeRecord, isShape: boolean): void {
  const knownFields: readonly string[] = hasNativeType(value.type)
    ? FIELDS[value.type]
    : ['children'];
  const allowed = isShape ? knownFields : ['children'];

  for (const key of Object.keys(value)) {
    if (!BASE.has(key) && !allowed.includes(key)) {
      fail(`Unsupported field ${key} on ${value.type}.`);
    }
  }

  if (value.children === undefined && allowed.includes('children')) {
    fail('Parent requires children.');
  }
}

/**
 * checks whether an application form can express the native construct
 * @param type native discriminator
 * @param form application form
 * @returns whether form and native category agree
 */
function isValidForm(type: string, form: unknown): boolean {
  if (type === 'html') {
    return form === 'inline' || form === 'block';
  }

  return form === (PHRASING.has(type) ? 'inline' : 'block');
}
