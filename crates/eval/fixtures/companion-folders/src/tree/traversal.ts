import type { MdcxNode, NativeNode } from '#types';

/** preorder visitor */
export type NodeVisitor = (context: VisitContext) => void;

/** traversal context */
export interface VisitContext {
  node: MdcxNode<string>;
  path: NodePath;
  parent?: MdcxNode<string>;
}

/** structural path in a document snapshot */
export type NodePath = ReadonlyArray<'children' | number>;

/**
 * visits each occurrence exactly once in preorder
 * @param root canonical document root
 * @param visit preorder callback receiving node and path
 */
export function walk(root: MdcxNode<string>, visit: NodeVisitor): void {
  const pending: VisitContext[] = [{ node: root, path: [] }];
  let current = pending.pop();

  while (current !== undefined) {
    visit(current);

    for (const [index, node] of Array.from(
      getChildren(current.node).entries(),
    ).reverse()) {
      pending.push({
        node,
        parent: current.node,
        path: [...current.path, 'children', index],
      });
    }

    current = pending.pop();
  }
}

/**
 * extracts namespace-scoped stable identity
 * @param node canonical semantic node
 * @returns validated semantic result
 */
export function getIdentity(
  node: MdcxNode<string>,
): readonly [string, string] | undefined {
  return node.ref === undefined ? undefined : [node.kind ?? 'common', node.ref];
}

/**
 * extracts semantic text through the structural axis
 * @param node canonical semantic node
 * @returns validated semantic result
 */
export function getTextContent(node: MdcxNode<string>): string {
  return 'value' in node
    ? node.value
    : 'alt' in node
      ? (node.alt ?? '')
      : getChildren(node).map(getTextContent).join('');
}

/**
 * traverses the single structural axis
 * @param node canonical semantic node
 * @returns validated semantic result
 */
export function getChildren<A extends string>(
  node: MdcxNode<A>,
): ReadonlyArray<MdcxNode<A>> {
  return 'children' in node ? node.children : [];
}

/**
 * narrows a checked canonical node to its native namespace
 * @param node canonical semantic node
 * @returns validated semantic result
 */
export function isNativeNode<A extends string>(
  node: MdcxNode<A>,
): node is NativeNode<A> {
  return (
    (node.kind === undefined || node.kind === 'common') &&
    node.form === undefined
  );
}
