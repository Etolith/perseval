#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
FAILED=0

fail() {
  echo "release-readiness: $*" >&2
  FAILED=1
}

test -f "$ROOT/LICENSE" || fail "root LICENSE is missing"

if grep -Eq 'traces-to-evals[[:space:]]*=[[:space:]]*\{[[:space:]]*path[[:space:]]*=' "$ROOT/Cargo.toml"; then
  fail "traces-to-evals still uses an unpublished sibling path"
fi

if git -C "$ROOT" ls-files docs | grep -q .; then
  fail "internal docs are still tracked; move reviewed public material to an explicit public path"
fi

if git -C "$ROOT" ls-files | grep -Eq '(^|/)\.env($|\.)|(^|/)(benchmarks|design)/'; then
  fail "private environment, benchmark, or design material is tracked"
fi

if git -C "$ROOT" grep -n -E '(/Users/[^/ ]+|[A-Za-z0-9._%+-]+@gmail\.com|sk-[A-Za-z0-9_-]{16,}|OPENAI_API_KEY=.)' -- . ':!Cargo.lock' ':!scripts/check-release-readiness.sh'; then
  fail "tracked files contain a personal path, email, or credential-shaped value"
fi

if find "$ROOT/crates/perseval-service/examples" -type f -print -quit 2>/dev/null | grep -q .; then
  fail "one-off service examples remain"
fi

if test -f "$ROOT/apps/perseval-app/src/screens/runs/legacy.rs"; then
  fail "the unreachable legacy Runs screen remains"
fi

if [ "$FAILED" -ne 0 ]; then
  exit 1
fi

echo "release-readiness: public-tree preflight passed"
