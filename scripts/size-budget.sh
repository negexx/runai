#!/usr/bin/env bash
# scripts/size-budget.sh — the runtime's shipping size, and what it may not contain.
#
# The budget is 6144 bytes gzipped. It is a gate, not a report: a number over it
# fails the run, and the fix is to remove features or dev instrumentation — never
# to raise the number.
#
# Two facts make the measurement meaningful:
#
#   * The runtime's dev-only checks sit behind `process.env.NODE_ENV !== "production"`,
#     and the constant is only folded out when esbuild runs with `--minify`. The
#     production intent is therefore stated with an explicit `--define` as well:
#     esbuild's `--minify` defaults the constant to `"production"` even when the
#     shell says otherwise (verified: `NODE_ENV=development` with `--minify` still
#     emits 2194 bytes and no `RUNAI_DEV`), so relying on the implicit default would
#     leave the gate quietly measuring whatever esbuild decides next.
#   * The dev guards are therefore **absent**, not merely small, and asserting that
#     is the second half of the gate. Sizes alone would not catch a `RUNAI_DEV`
#     branch that survived a refactor into a shape esbuild stops folding.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/packages/runai"

budget=6144
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
min="$work/runtime.min.js"

# `bunx` rather than a dependency: esbuild is a tool of this check, not something
# the runtime ships, and packages/runai has no build step of its own.
NODE_ENV=production bunx esbuild runtime.js --minify --format=esm \
  --define:process.env.NODE_ENV='"production"' --log-level=warning --outfile="$min"

# `gzip -9 -n` on the minified file, as a browser's transfer encoding would see it.
# `-n` is not cosmetic: without it gzip stores the input's basename and mtime in
# the header, and the count moved between 2194 and 2209 bytes depending on the
# temporary directory's name. A budget has to measure the payload, not the path.
# Bytes, not kilobytes: a budget in KB would hide 1023 bytes of growth.
gzip -9 -n -c "$min" > "$min.gz"
bytes=$(wc -c < "$min.gz" | tr -d ' ')

# Dev instrumentation, by name. Set -e exempts a bare `! grep`, so this is an `if`.
if grep -q 'RUNAI_DEV' "$min"; then
  echo "error: the production runtime still carries dev instrumentation (RUNAI_DEV):"
  grep -o '.\{0,60\}RUNAI_DEV.\{0,60\}' "$min" | head -5
  exit 1
fi

echo "runtime gzipped: ${bytes} bytes (budget ${budget})"
test "$bytes" -le "$budget" || {
  echo "error: the runtime is ${bytes} bytes gzipped, over the ${budget} byte budget"
  echo "       remove features or dev instrumentation; do not raise the budget"
  exit 1
}
echo "size ok"
