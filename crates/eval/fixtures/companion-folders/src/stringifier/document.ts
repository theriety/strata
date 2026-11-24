import { stringifyDocumentMapping } from '#parser/annotation';

import { StringifyError } from './errors';

import { hasMetadata } from './metadata';

import type { MdcxRoot } from '#types';

import type { WriterContext } from './types';

const MAXIMUM_FRONT_MATTER_BYTES = 65536;

/**
 * emits a single authoritative document metadata mapping
 * @param root semantic document
 * @param context output configuration
 * @returns canonical front matter or an empty string
 */
export function stringifyDocumentMetadata(
  root: MdcxRoot<string>,
  context: WriterContext,
): string {
  if (context.profile === 'commonmark') {
    if (hasMetadata(root) || root.form === 'document') {
      throw new StringifyError(
        'CommonMark cannot represent document application metadata.',
      );
    }

    return '';
  }

  if (!context.includes.includes('meta')) {
    return '';
  }

  if (!hasMetadata(root) && root.type === 'root' && root.form !== 'document') {
    return '';
  }

  const metadata = {
    ...root.attributes,
    kind: root.kind ?? 'common',
    type: root.type,
  };
  const mapping = stringifyDocumentMapping(
    root.ref === undefined ? metadata : { ...metadata, ref: root.ref },
  );

  if (new TextEncoder().encode(mapping).length > MAXIMUM_FRONT_MATTER_BYTES) {
    throw new StringifyError('Front matter exceeds 65536 UTF-8 bytes.');
  }

  return ['---', mapping, '---'].join('\n');
}
