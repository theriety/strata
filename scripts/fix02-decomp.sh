#!/bin/zsh
# FIX02 working tool: per-fixture J decomposition summary from the
# strata-eval diagnostics dumps (STRATA_BLESS_EVAL=1 output).
# Usage: scripts/fix02-decomp.sh [fixture ...]   (default: all seven)
set -euo pipefail

diagnostics="$PWD/target/strata-eval-diagnostics"
fixtures=("$@")
if (( ${#fixtures[@]} == 0 )); then
  fixtures=(collapse inversion large-app naming-drift relief tearing welding)
fi

for fixture in "${fixtures[@]}"; do
  file="$diagnostics/$fixture.json"
  if [[ ! -f $file ]]; then
    echo "== $fixture == MISSING DUMP ($file)"
    continue
  fi
  echo "== $fixture =="
  jq -r '"current: cut=\(.result.currentScoreBreakdown.cut) imb=\(.result.currentScoreBreakdown.imbalance) nam=\(.result.currentScoreBreakdown.naming) path=\(.result.currentScoreBreakdown.path) anch=\(.result.currentScoreBreakdown.anchor) tot=\(.result.currentScoreBreakdown.total)"' "$file"
  jq -r '.result.modes | to_entries[] | .key as $m | .value.candidates | to_entries[] | "  \($m)#\(.key+1): cut=\(.value.scoreBreakdown.cut) imb=\(.value.scoreBreakdown.imbalance) nam=\(.value.scoreBreakdown.naming) path=\(.value.scoreBreakdown.path) anch=\(.value.scoreBreakdown.anchor) tot=\(.value.scoreBreakdown.total) imp=\(.value.improvement)"' "$file"
  failed=$(jq -r '.verdicts[] | select(.passed==false) | "  FAIL \(.label): \(.detail)"' "$file")
  if [[ -n $failed ]]; then
    print -r -- "$failed"
  else
    echo "  all verdicts green"
  fi
done
