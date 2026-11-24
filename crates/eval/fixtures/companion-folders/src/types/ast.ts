/** canonical document union */
export type MdcxRoot<A extends string = never> = Root<A> | ApplicationRoot<A>;

/** complete canonical semantic node union */
export type MdcxNode<A extends string = never> =
  | NativeNode<A>
  | ApplicationNode<A>;

/** native semantic node union */
export type NativeNode<A extends string = never> =
  | Root<A>
  | NativeFlowContent<A>
  | NativePhrasingContent<A>
  | ListItem<A>
  | TableRow<A>
  | TableCell<A>;

/** native Markdown document */
export interface Root<A extends string = never> extends NativeMetadata {
  type: 'root';
  children: Array<MdcxFlowContent<A>>;
}

/** complete application node union */
export type ApplicationNode<A extends string = never> =
  | ApplicationRoot<A>
  | ApplicationFlowContent<A>
  | ApplicationPhrasingContent<A>
  | ApplicationShape<ListItem<A> | TableRow<A> | TableCell<A>, A>;

/** application document classification */
export interface ApplicationRoot<
  A extends string = never,
> extends ApplicationMetadata<A> {
  type: string;
  form: 'document';
  children: Array<MdcxFlowContent<A>>;
}

/** quote containing flow and definitions */
export interface Blockquote<A extends string = never> extends NativeMetadata {
  type: 'blockquote';
  children: Array<MdcxFlowContent<A>>;
}

/** list with restricted item children */
export interface List<A extends string = never> extends NativeMetadata {
  type: 'list';
  children: Array<MdcxListItem<A>>;
  ordered?: boolean | null;
  start?: number | null;
  spread?: boolean | null;
}

/** list item, optionally a GFM task */
export interface ListItem<A extends string = never> extends NativeMetadata {
  type: 'listItem';
  children: Array<MdcxFlowContent<A>>;
  spread?: boolean | null;
  checked?: boolean | null;
}

/** native recursive flow union */
export type NativeFlowContent<A extends string = never> =
  | Paragraph<A>
  | Heading<A>
  | Blockquote<A>
  | List<A>
  | Code
  | Math
  | Html
  | ThematicBreak
  | Definition
  | Table<A>;

/** named application block */
export interface CustomBlock<
  A extends string = never,
> extends ApplicationMetadata<A> {
  type: string;
  form: 'block';
  children: Array<MdcxFlowContent<A>>;
}

/** application flow retaining native semantic fields */
export type ApplicationFlowContent<A extends string = never> =
  | ApplicationShape<NativeFlowContent<A>, A>
  | CustomBlock<A>;

/** restricted list children */
export type MdcxListItem<A extends string = never> =
  | ListItem<A>
  | ApplicationShape<ListItem<A>, A>;

/** recursive MDCX flow including definitions */
export type MdcxFlowContent<A extends string = never> =
  | NativeFlowContent<A>
  | ApplicationFlowContent<A>;

/** block separator */
export interface ThematicBreak extends NativeMetadata {
  type: 'thematicBreak';
}

/** fenced or indented literal code */
export interface Code extends NativeMetadata {
  type: 'code';
  value: string;
  lang?: string | null;
  meta?: string | null;
}

/** literal display mathematics */
export interface Math extends NativeMetadata {
  type: 'math';
  value: string;
  meta?: string | null;
}

/** resource definition */
export interface Definition extends NativeMetadata, Association {
  type: 'definition';
  url: string;
  title?: string | null;
}

/** paragraph with recursive phrasing */
export interface Paragraph<A extends string = never> extends NativeMetadata {
  type: 'paragraph';
  children: Array<MdcxPhrasingContent<A>>;
}

/** heading with recursive phrasing */
export interface Heading<A extends string = never> extends NativeMetadata {
  type: 'heading';
  depth: HeadingDepth;
  children: Array<MdcxPhrasingContent<A>>;
}

/** heading level */
export type HeadingDepth = 1 | 2 | 3 | 4 | 5 | 6;

/** GFM table whose first row is its header */
export interface Table<A extends string = never> extends NativeMetadata {
  type: 'table';
  children: Array<MdcxTableRow<A>>;
  align?: Align[] | null;
  /** true when the first row is a GFM header; false for headerless tables */
  hasHeader?: boolean;
}

/** GFM column alignment */
export type Align = 'left' | 'right' | 'center' | null;

/** restricted table children */
export type MdcxTableRow<A extends string = never> =
  | TableRow<A>
  | ApplicationShape<TableRow<A>, A>;

/** table row with restricted cell children */
export interface TableRow<A extends string = never> extends NativeMetadata {
  type: 'tableRow';
  children: Array<MdcxTableCell<A>>;
}

/** restricted row children */
export type MdcxTableCell<A extends string = never> =
  | TableCell<A>
  | ApplicationShape<TableCell<A>, A>;

/** table cell with phrasing content */
export interface TableCell<A extends string = never> extends NativeMetadata {
  type: 'tableCell';
  children: Array<MdcxPhrasingContent<A>>;
}

/** recursively immutable resolver input */
export type DeepReadonly<T> = T extends object
  ? { readonly [K in keyof T]: DeepReadonly<T[K]> }
  : T;

/** immutable semantic mapping supplied to resolvers */
export type ReadonlyAttributes = Readonly<Record<string, ReadonlyJsonValue>>;

/** recursively immutable JSON value */
export type ReadonlyJsonValue =
  | null
  | boolean
  | number
  | string
  | readonly ReadonlyJsonValue[]
  | { readonly [key: string]: ReadonlyJsonValue };

/** GFM table-cell phrasing excludes explicit hard line breaks */
export type MdcxTablePhrasingContent<A extends string = never> =
  | Exclude<NativePhrasingContent<A>, Break>
  | ApplicationShape<Exclude<NativePhrasingContent<A>, Break>, A, 'inline'>
  | CustomInline<A>;

/** recursive emphasis */
export interface Emphasis<A extends string = never> extends NativeMetadata {
  type: 'emphasis';
  children: Array<MdcxPhrasingContent<A>>;
}

/** recursive strong emphasis */
export interface Strong<A extends string = never> extends NativeMetadata {
  type: 'strong';
  children: Array<MdcxPhrasingContent<A>>;
}

/** GFM strikethrough */
export interface Delete<A extends string = never> extends NativeMetadata {
  type: 'delete';
  children: Array<MdcxPhrasingContent<A>>;
}

/** link resource with recursive label */
export interface Link<A extends string = never> extends NativeMetadata {
  type: 'link';
  url: string;
  children: Array<MdcxPhrasingContent<A>>;
  title?: string | null;
}

/** link reference with recursive label */
export interface LinkReference<A extends string = never>
  extends NativeMetadata, Association {
  type: 'linkReference';
  referenceType: ReferenceType;
  children: Array<MdcxPhrasingContent<A>>;
}

/** native recursive phrasing union */
export type NativePhrasingContent<A extends string = never> =
  | Text
  | InlineCode
  | InlineMath
  | Html
  | Break
  | Image
  | ImageReference
  | Emphasis<A>
  | Strong<A>
  | Delete<A>
  | Link<A>
  | LinkReference<A>;

/** named application inline construct */
export interface CustomInline<
  A extends string = never,
> extends ApplicationMetadata<A> {
  type: string;
  form: 'inline';
  children: Array<MdcxPhrasingContent<A>>;
}

/** application phrasing retaining native semantic fields */
export type ApplicationPhrasingContent<A extends string = never> =
  | ApplicationShape<NativePhrasingContent<A>, A, 'inline'>
  | CustomInline<A>;

/** recursive MDCX phrasing */
export type MdcxPhrasingContent<A extends string = never> =
  | NativePhrasingContent<A>
  | ApplicationPhrasingContent<A>;

/** standard literal text */
export interface Text extends NativeMetadata {
  type: 'text';
  value: string;
}

/** opaque code span */
export interface InlineCode extends NativeMetadata {
  type: 'inlineCode';
  value: string;
}

/** literal inline mathematics */
export interface InlineMath extends NativeMetadata {
  type: 'inlineMath';
  value: string;
}

/** literal HTML */
export interface Html extends NativeMetadata {
  type: 'html';
  value: string;
}

/** hard line break */
export interface Break extends NativeMetadata {
  type: 'break';
}

/** inline image with textual alternative */
export interface Image extends NativeMetadata {
  type: 'image';
  url: string;
  title?: string | null;
  alt?: string | null;
}

/** associates textual alternative content with a reference definition */
export interface ImageReference extends NativeMetadata, Association {
  type: 'imageReference';
  referenceType: ReferenceType;
  alt?: string | null;
}

/** reference association */
export interface Association {
  identifier: string;
  label?: string | null;
}

/** preserves whether a reference uses full, collapsed, or shortcut syntax */
export type ReferenceType = 'shortcut' | 'collapsed' | 'full';

/** application envelope */
export interface ApplicationMetadata<
  A extends string = never,
> extends NodeMetadata {
  kind: ApplicationKind<A>;
  form: 'block' | 'inline' | 'document';
}

/** rebases a native shape onto an application namespace */
export type ApplicationShape<
  N,
  A extends string = never,
  F extends 'block' | 'inline' = 'block',
> = N extends NativeMetadata
  ? Omit<N, 'kind' | 'form'> & { kind: ApplicationKind<A>; form: F }
  : never;

/** ordinary Markdown envelope */
export interface NativeMetadata extends NodeMetadata {
  kind?: 'common';
  form?: never;
}

/** persistent metadata shared by semantic nodes */
export interface NodeMetadata {
  ref?: string;
  attributes?: Attributes;
  range?: SourceRange;
  data?: Record<string, unknown>;
}

/** typed semantic attribute mapping */
export type Attributes = Record<string, JsonValue>;

/** JSON-compatible semantic attribute value */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

/** end-exclusive source extent */
export interface SourceRange {
  start: SourcePosition;
  end: SourcePosition;
}

/** original UTF-16 source coordinate */
export interface SourcePosition {
  line: number;
  column: number;
  offset?: number;
}

/** namespace literals; open strings are checked at runtime */
export type ApplicationKind<A extends string = never> =
  | 'custom'
  | Exclude<A, 'common' | ''>;

/** preserves finite option namespaces alongside checked open source selectors */
export type ParsedNamespace<A extends string = never> =
  | RuntimeNamespace
  | (string extends A ? never : A);

/** open application namespace admitted by semantic validation */
export type RuntimeNamespace = string & { readonly [runtimeNamespace]: true };

/** nominal marker for an application namespace checked at runtime */
declare const runtimeNamespace: unique symbol;
