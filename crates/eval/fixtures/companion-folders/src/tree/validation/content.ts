import { isMarkdownShape } from '../validation';

import { FLOW, PHRASING } from './shape';

import type {
  ApplicationShape,
  MdcxNode,
  NativeFlowContent,
  NativePhrasingContent,
} from '#types';

/** complete Markdown flow shapes, excluding restricted structural collections */
export type MarkdownFlowShape =
  | NativeFlowContent<string>
  | ApplicationShape<NativeFlowContent<string>, string>;

/** complete Markdown phrasing shapes with their original application envelope */
export type MarkdownPhrasingShape =
  | NativePhrasingContent<string>
  | ApplicationShape<NativePhrasingContent<string>, string, 'inline'>;

/**
 * narrows native body fields and their permitted flow category together
 * @param value canonical node with an independently validated application envelope
 * @returns whether the complete node is a Markdown flow shape
 */
export function isMarkdownFlowShape(
  value: MdcxNode<string>,
): value is MarkdownFlowShape {
  return (
    FLOW.has(value.type) && value.form !== 'inline' && isMarkdownShape(value)
  );
}

/**
 * narrows native inline fields without admitting block application forms
 * @param value canonical node with an independently validated application envelope
 * @returns whether the complete node is a Markdown phrasing shape
 */
export function isMarkdownPhrasingShape(
  value: MdcxNode<string>,
): value is MarkdownPhrasingShape {
  return (
    PHRASING.has(value.type) &&
    value.form !== 'block' &&
    value.form !== 'document' &&
    isMarkdownShape(value)
  );
}
