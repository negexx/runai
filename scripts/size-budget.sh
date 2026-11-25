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
# The artifact is written to an **absolute** path inside the workspace, not into the
# shell's temporary directory. The old `mktemp -d` relied on the tool honouring a path
# outside the tree: on this machine `bunx` resolved to a Windows shim, esbuild wrote
# into `packages/runai/tmp/` instead of `/tmp/tmp.X/`, and the gate only noticed
# because `gzip` refuses a missing file. A path under the workspace is one the tool
# can always reach, and `.gitignore` covers `target/`, so a run leaves no tracked file.
# The absolute form is what makes a path-mangling tool fail the `[ -s ]` check below
# rather than write somewhere this script never reads.
out="$root/packages/runai/target/size-budget"
mkdir -p "$out"
min="$out/runtime.min.js"
# Delete any previous artifact first. Without this a *failed* esbuild would leave the
# last run's file in place and the gate would measure it — a stale number is the same
# false green as an empty one.
rm -f "$min" "$min.gz"

# `bunx` rather than a dependency: esbuild is a tool of this check, not something
# the runtime ships, and packages/runai has no build step of its own.
NODE_ENV=production bunx esbuild runtime.js --minify --format=esm \
  --define:process.env.NODE_ENV='"production"' --log-level=warning --outfile="$min"

# The gate must assert the artifact it measures. An empty file gzips to 20 bytes, and
# `20 -le 6144` printed `size ok`, so the pass path was undefended: any run where
# esbuild produced nothing was a silent pass.
[ -s "$min" ] || {
  echo "error: esbuild produced no output at $min"
  exit 1
}

# A sanity floor, because "non-empty" is not the same as "the runtime". The minified
# runtime measured 5485 bytes when this floor was added, so 1000 is ~5x headroom: far
# below any real build of this file, and far above an empty file (0), a stub, or a
# truncated write. It exists to catch a tool that succeeds while emitting almost
# nothing — the case `[ -s ]` cannot see — without pinning the exact size, which the
# budget below already does at a coarser scale.
min_bytes=$(wc -c < "$min" | tr -d ' ')
if [ "$min_bytes" -lt 1000 ]; then
  echo "error: $min is only ${min_bytes} bytes: esbuild emitted a stub, not the runtime"
  exit 1
fi

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
