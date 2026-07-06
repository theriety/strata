#!/usr/bin/env bash
# Demonstrates a full strata run: analyze once, then render every face from the saved result.
# Usage: ./demo.sh [target-repo] [out-dir]
set -euo pipefail

TARGET="${1:-$HOME/Repositories/notion-sync}"
OUT="${2:-/tmp/strata-demo}"
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/strata"

[ -d "$TARGET" ] || { echo "error: target repo not found: $TARGET" >&2; exit 1; }
[ -x "$BIN" ] || cargo build --release --manifest-path "$HERE/Cargo.toml" -p strata-cli

# use the target's own strata.toml when present; otherwise pin built-in defaults
# (a nonexistent --config falls back silently, shielding the demo from any ambient toml)
CFG="$TARGET/strata.toml"
[ -f "$CFG" ] || CFG="/nonexistent/strata-demo.toml"

mkdir -p "$OUT"
RESULT="$OUT/result.json"

echo "== analyze (both modes) =="
"$BIN" analyze --root "$TARGET" --config "$CFG" --mode both --format json --output "$RESULT"
echo "wrote $RESULT"

echo
echo "== current tree (depth 3) =="
"$BIN" tree --input "$RESULT" --current --depth 3

echo
echo "== proposed layout: anchored candidate 1 =="
"$BIN" tree --input "$RESULT" --mode anchored --candidate 1 --depth 3

echo
echo "== moves: current -> anchored/1 =="
"$BIN" diff --input "$RESULT" current anchored/1

echo
echo "== violations (CI gate: cycle,polarity) =="
rc=0
"$BIN" violations --root "$TARGET" --config "$CFG" --fail-on cycle,polarity || rc=$?
[ "$rc" -eq 0 ] || [ "$rc" -eq 2 ] || exit "$rc"
echo "gate exit code: $rc   (0 = clean, 2 = would fail CI)"

echo
echo "== markdown report =="
"$BIN" report --input "$RESULT" --output "$OUT/report.md"
echo "wrote $OUT/report.md"
