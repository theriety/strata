import {
  computeCombinedSimilarity,
  computeSetIntersectionSize,
  consumeSimilarityComparison,
} from './similarity';

import type { IndexedNode } from './indexer';
import type { TreeMatches } from './matcher';
import type { SimilarityBudget } from './similarity';

/** native occurrence matching context shared with sibling matching */
export interface InferMatchesParams extends TreeMatches {
  bind: (left: IndexedNode, right: IndexedNode) => void;
  budget: SimilarityBudget;
  settledSource: Set<IndexedNode>;
  settledTarget: Set<IndexedNode>;
}

/** candidate selection for one anonymous target */
interface InferCandidateParams {
  target: IndexedNode;
  candidates: IndexedNode[];
  context: InferMatchesParams;
  inference: InferenceContext;
}

/** unmatched snapshot context and inference-only correspondence */
interface InferenceContext {
  source: IndexedNode[];
  target: IndexedNode[];
  matches: Map<IndexedNode, IndexedNode>;
}

/** scored native occurrence */
interface ScoredCandidate {
  entry: IndexedNode;
  score: number;
}

/** content and structural thresholds retained from the original matcher */
const CONTENT_THRESHOLD = 0.6;
const STRUCTURAL_THRESHOLD = 0.3;
const POSITION_THRESHOLD = 0.05;
const FINGERPRINT_THRESHOLD = 0.6;
const FALLBACK_THRESHOLD = 0.5;
const TYPE_WEIGHT = 0.4;
const TEXT_WEIGHT = 0.6;
const CONTEXT_WEIGHT = 0.2;
const ROOT_CHILD_PATH_LENGTH = 2;

/**
 * infers missing references and reconciles edited references without assigning identity
 * @param context native snapshots, exact matches and shared comparison budget
 */
export function inferMatches(context: InferMatchesParams): void {
  const {
    source,
    target,
    forward,
    reverse,
    bind,
    settledSource,
    settledTarget,
  } = context;
  const inference: InferenceContext = {
    source: source.filter(
      (entry) =>
        entry.parent &&
        !forward.has(entry) &&
        !settledSource.has(entry) &&
        entry.path.length === ROOT_CHILD_PATH_LENGTH,
    ),
    target: target.filter(
      (entry) =>
        entry.parent &&
        !reverse.has(entry) &&
        !settledTarget.has(entry) &&
        entry.path.length === ROOT_CHILD_PATH_LENGTH,
    ),
    matches: new Map(),
  };

  for (const right of inference.target) {
    if (right.identity || reverse.has(right) || settledTarget.has(right)) {
      continue;
    }

    const candidates = source.filter(
      (left) =>
        left.parent &&
        left.identity &&
        !forward.has(left) &&
        !settledSource.has(left) &&
        sameNamespace(left, right) &&
        getForm(left) === getForm(right) &&
        left.path.length === ROOT_CHILD_PATH_LENGTH,
    );
    const candidate = inferCandidate({
      target: right,
      candidates,
      context,
      inference,
    });

    if (candidate) {
      inference.matches.set(right, candidate);
      bind(candidate, right);
    }
  }

  matchRemaining(context);
}

/**
 * matches remaining occurrences above the original weighted similarity threshold
 * @param context native snapshots and shared comparison budget
 */
function matchRemaining(context: InferMatchesParams): void {
  const { target, reverse, bind, settledTarget } = context;
  for (const right of target) {
    if (
      !right.parent ||
      reverse.has(right) ||
      settledTarget.has(right) ||
      right.path.length !== ROOT_CHILD_PATH_LENGTH
    ) {
      continue;
    }

    const best = findFallbackCandidate(right, context);
    if (best) {
      bind(best, right);
    }
  }
}

/**
 * ranks all remaining candidates while charging incompatible shapes to the budget
 * @param right target occurrence
 * @param context native source candidates and shared budget
 * @returns best compatible source occurrence above the fallback threshold
 */
function findFallbackCandidate(
  right: IndexedNode,
  context: InferMatchesParams,
): IndexedNode | undefined {
  const { source, forward, budget, settledSource } = context;
  let best: ScoredCandidate | undefined;
  const candidates = source.filter(
    (left) =>
      left.parent &&
      !forward.has(left) &&
      !settledSource.has(left) &&
      left.path.length === ROOT_CHILD_PATH_LENGTH,
  );
  for (const left of candidates) {
    if (!consumeSimilarityComparison(budget)) {
      break;
    }
    if (!sameShape(left, right)) {
      continue;
    }
    const score =
      TYPE_WEIGHT +
      TEXT_WEIGHT * computeCombinedSimilarity(left.text, right.text);
    if (score > FALLBACK_THRESHOLD && (!best || score > best.score)) {
      best = { entry: left, score };
    }
  }

  return best?.entry;
}

/**
 * retains positional, content, structural and contextual inference precedence
 * @param params one target and remaining referenced source candidates
 * @returns inferred source occurrence when a strategy qualifies
 */
function inferCandidate(params: InferCandidateParams): IndexedNode | undefined {
  const { target, candidates, context } = params;
  const positional = findPositionalCandidate(params);

  if (
    positional &&
    consumeSimilarityComparison(context.budget) &&
    computeCombinedSimilarity(positional.text, target.text) >=
      POSITION_THRESHOLD
  ) {
    return positional;
  }

  const compatible = candidates.filter((candidate) =>
    sameShape(candidate, target),
  );
  const content = findContentCandidate({ ...params, candidates: compatible });
  if (content) {
    return content;
  }
  if (
    compatible.length === 1 &&
    consumeSimilarityComparison(context.budget) &&
    computeCombinedSimilarity(compatible[0]!.text, target.text) >=
      STRUCTURAL_THRESHOLD
  ) {
    return compatible[0];
  }

  let fingerprint: ScoredCandidate | undefined;
  for (const entry of candidates) {
    const score = computeContextSimilarity(entry, params);
    if (
      score >= FINGERPRINT_THRESHOLD &&
      (!fingerprint || score > fingerprint.score)
    ) {
      fingerprint = { entry, score };
    }
  }

  return fingerprint?.entry;
}

/**
 * selects the highest scoring nonempty content candidate above the original threshold
 * @param params compatible source candidates and shared comparison budget
 * @returns best matching source occurrence when content inference succeeds
 */
function findContentCandidate(
  params: InferCandidateParams,
): IndexedNode | undefined {
  const { target, candidates, context } = params;
  let best: ScoredCandidate | undefined;
  for (const entry of candidates.filter(
    (candidate) => candidate.text.length > 0 && target.text.length > 0,
  )) {
    if (!consumeSimilarityComparison(context.budget)) {
      break;
    }
    const score = computeCombinedSimilarity(entry.text, target.text);
    if (score >= CONTENT_THRESHOLD && (!best || score > best.score)) {
      best = { entry, score };
    }
  }

  return best?.entry;
}

/**
 * locates a unique source occurrence after the previous inferred target
 * @param params anonymous target and available source occurrences
 * @returns unique candidate inside the matched positional gap
 */
function findPositionalCandidate(
  params: InferCandidateParams,
): IndexedNode | undefined {
  const { target, candidates, inference } = params;
  const index = inference.target.indexOf(target);
  const previous = inference.target
    .slice(0, index)
    .reverse()
    .map((entry) => inference.matches.get(entry))
    .find((entry) => entry !== undefined);
  if (!previous) {
    return undefined;
  }
  const lower = inference.source.indexOf(previous);
  const within = candidates.filter(
    (entry) => inference.source.indexOf(entry) > lower,
  );

  return within.length === 1 ? within[0] : undefined;
}

/**
 * compares native namespace boundaries
 * @param left source occurrence
 * @param right target occurrence
 * @returns whether inference stays within the namespace
 */
function sameNamespace(left: IndexedNode, right: IndexedNode): boolean {
  return (left.node.kind ?? 'common') === (right.node.kind ?? 'common');
}

/**
 * resolves the form of an eligible root child without revalidating the snapshot
 * @param entry validated root child
 * @returns explicit application form or native block form
 */
function getForm(entry: IndexedNode): string {
  return entry.node.form ?? 'block';
}

/**
 * compares classification and form within the native namespace
 * @param left source occurrence
 * @param right target occurrence
 * @returns whether content similarity can match the occurrences
 */
function sameShape(left: IndexedNode, right: IndexedNode): boolean {
  return (
    sameNamespace(left, right) &&
    left.node.type === right.node.type &&
    left.node.form === right.node.form
  );
}

/**
 * scores native type, parent, depth and sibling context with original weights
 * @param left source occurrence
 * @param params target occurrence and unmatched snapshot context
 * @returns contextual similarity from zero to one
 */
function computeContextSimilarity(
  left: IndexedNode,
  params: InferCandidateParams,
): number {
  const { target: right, inference } = params;
  const leftTypes = new Set(
    inference.source
      .filter((entry) => entry !== left)
      .map((entry) => entry.node.type),
  );
  const rightTypes = new Set(
    inference.target
      .filter((entry) => entry !== right)
      .map((entry) => entry.node.type),
  );
  const intersection = computeSetIntersectionSize(leftTypes, rightTypes);
  const union = leftTypes.size + rightTypes.size - intersection;

  // scoped children are already settled; remaining occurrences share root depth and parent context
  return (
    (sameShape(left, right) ? TYPE_WEIGHT : 0) +
    CONTEXT_WEIGHT +
    CONTEXT_WEIGHT +
    (union ? intersection / union : 1) * CONTEXT_WEIGHT
  );
}
