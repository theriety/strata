import { indexTree } from './indexer';
import { inferMatches } from './inference';
import {
  computeCombinedSimilarity,
  consumeSimilarityComparison,
  createSimilarityBudget,
} from './similarity';

import type { MdcxRoot } from '#types/ast';

import type { IndexedNode } from './indexer';
import type { SimilarityBudget } from './similarity';

/** occurrence matches never alter or invent persistent identity */
export interface TreeMatches {
  source: IndexedNode[];
  target: IndexedNode[];
  forward: Map<IndexedNode, IndexedNode>;
  reverse: Map<IndexedNode, IndexedNode>;
}

/** sibling matching inputs */
interface MatchChildrenParams {
  budget: SimilarityBudget;
  left: IndexedNode;
  right: IndexedNode;
  forward: Map<IndexedNode, IndexedNode>;
  reverse: Map<IndexedNode, IndexedNode>;
  bind: (left: IndexedNode, right: IndexedNode) => void;
}

/** bounded candidate ranking inputs */
interface PreferSimilarParams {
  candidates: IndexedNode[];
  target: IndexedNode;
  budget: SimilarityBudget;
}

/** identity phase inputs */
interface IdentityMatchesParams {
  source: IndexedNode[];
  target: IndexedNode[];
  bind: (left: IndexedNode, right: IndexedNode) => void;
}

/**
 * matches identities, scoped children and inferred native occurrences deterministically
 * @param sourceRoot validated original document
 * @param targetRoot validated edited document
 * @returns snapshot indexes and bidirectional occurrence matches
 */
export function matchTrees(
  sourceRoot: MdcxRoot<string>,
  targetRoot: MdcxRoot<string>,
): TreeMatches {
  const source = indexTree(sourceRoot);
  const target = indexTree(targetRoot);
  const forward = new Map<IndexedNode, IndexedNode>();
  const reverse = new Map<IndexedNode, IndexedNode>();
  const pending: Array<readonly [IndexedNode, IndexedNode]> = [];
  const settledSource = new Set<IndexedNode>();
  const settledTarget = new Set<IndexedNode>();
  const bind = (left: IndexedNode, right: IndexedNode): void => {
    forward.set(left, right);
    reverse.set(right, left);
    pending.push([left, right]);
  };
  matchIdentities({ source, target, bind });

  const budget = createSimilarityBudget();

  let cursor = 0;
  const matchPendingChildren = (): void => {
    while (cursor < pending.length) {
      const [left, right] = pending[cursor++]!;
      if (left.parent && right.parent) {
        matchChildren({ left, right, forward, reverse, bind, budget });
        left.children.forEach((entry) => settledSource.add(entry));
        right.children.forEach((entry) => settledTarget.add(entry));
      }
    }
  };
  matchPendingChildren();
  inferMatches({
    source,
    target,
    forward,
    reverse,
    budget,
    settledSource,
    settledTarget,
    bind: (left, right): void => {
      bind(left, right);
      matchPendingChildren();
    },
  });

  return { source, target, forward, reverse };
}

/**
 * matches namespace identities while keeping document identity separate
 * @param params native indexes and occurrence binding callback
 */
function matchIdentities(params: IdentityMatchesParams): void {
  const { source, target, bind } = params;
  const byIdentity = new Map(
    source
      .filter((entry) => entry.identity && entry.parent)
      .map((entry) => [JSON.stringify(entry.identity), entry]),
  );
  for (const right of target) {
    const left =
      right.identity && right.parent
        ? byIdentity.get(JSON.stringify(right.identity))
        : undefined;
    if (left) {
      bind(left, right);
    }
  }
  const leftRoot = source[0]!;
  const rightRoot = target[0]!;
  if (
    JSON.stringify(leftRoot.identity) === JSON.stringify(rightRoot.identity)
  ) {
    bind(leftRoot, rightRoot);
  }
}

/**
 * preserves equal child sequences and greedily pairs other children within budget
 * @param params matched parents and occurrence bindings
 */
function matchChildren(params: MatchChildrenParams): void {
  const { left, right, forward, reverse, bind, budget } = params;

  const equal =
    left.children.length === right.children.length &&
    left.children.every(
      (child, index) =>
        child.fingerprint === right.children[index]!.fingerprint,
    );
  for (const [index, child] of right.children.entries()) {
    if (reverse.has(child)) {
      continue;
    }
    const candidate = equal
      ? left.children[index]
      : preferSimilar({
          candidates: left.children.filter((entry) => !forward.has(entry)),
          target: child,
          budget,
        });
    if (candidate) {
      bind(candidate, child);
    }
  }
}

/**
 * groups compatible occurrences without unsafe native narrowing
 * @param entry indexed occurrence
 * @returns stable namespace, classification and form key
 */
function getShapeKey(entry: IndexedNode): string {
  return JSON.stringify([
    entry.node.kind ?? 'common',
    entry.node.type,
    entry.node.form,
  ]);
}

/**
 * prioritizes the closest edited text within the shared comparison budget
 * @param params unmatched same-shape candidates and shared work budget
 * @returns highest scoring compatible occurrence, retaining the first tie
 */
function preferSimilar(params: PreferSimilarParams): IndexedNode | undefined {
  const { candidates, target, budget } = params;
  let best: IndexedNode | undefined;
  let score = -1;

  for (const candidate of candidates) {
    if (!consumeSimilarityComparison(budget)) {
      break;
    }

    if (getShapeKey(candidate) !== getShapeKey(target)) {
      continue;
    }
    const similarity = computeCombinedSimilarity(candidate.text, target.text);

    if (similarity > score) {
      score = similarity;
      best = candidate;
    }
  }

  return best;
}
