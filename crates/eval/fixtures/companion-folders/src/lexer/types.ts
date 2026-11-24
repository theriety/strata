import type { ParseOptions } from '#parser/types';

import type { HeadingDepth, Adapter, MdcxNode } from '#types';

import type { Attributes, SourceRange } from '#types/ast';

/** an existing current line selected by the owning source iterator */
export interface BlockSource {
  lines: readonly SourceLine[];
  index: number;
  line: SourceLine;
  context: SourceContext;
}

/** original source and shared parser state. locate accepts original offsets */
export interface SourceContext {
  bypassAdapter?: Adapter<MdcxNode<string>>;
  source: string;
  budget: ParseBudget;
  definitions: Map<string, Definition>;
  options: ParseOptions;
  locate(start: number, end: number): SourceRange;
}

/** shared recursion accounting across all syntax contexts */
export interface ParseBudget {
  depth: number;
  limit: number;
  enter(): void;
  leave(): void;
}

/** physical line excluding its line ending */
export interface SourceLine {
  view: SourceView;
  end: number;
  isLazy?: boolean;
}

/** positioned block syntax resolved before semantic construction */
export interface BlockEvent {
  type: string;
  view: SourceView;
  namespace?: string;
  attributes?: Attributes;
  ref?: string;
  isCustom?: boolean;
  /** lexical body without its owning annotation envelope */
  body?: SourceView;
  /** explicit source type selector, distinct from inferred native type */
  selector?: string;
  shouldMergeList?: boolean;
  listMarker?: string;
  children?: BlockEvent[];
  inline?: SourceView;
  value?: string;
  depth?: HeadingDepth;
  ordered?: boolean;
  start?: number;
  spread?: boolean;
  checked?: boolean | null;
  lang?: string | null;
  meta?: string | null;
  definition?: Definition;
  align?: Array<'left' | 'right' | 'center' | null>;
  hasHeader?: boolean;
}

/** a source region with a mapping for every UTF-16 boundary */
export interface SourceView {
  text: string;
  /** reads an integer boundary in the inclusive range 0..text.length */
  mapOffset(index: number): number;
}

/** document-level Markdown reference association */
export interface Definition {
  identifier: string;
  url: string;
  label?: string;
  title?: string | null;
}
