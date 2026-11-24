#!/usr/bin/env bash
# scripts/e2e.sh — the whole slice, end to end.
#
# Builds `examples/counter` with the real CLI, asserts the published tree is the
# one this framework promises (an island that ships JavaScript, a static route that
# ships none), then runs the built island under a DOM and clicks it. `smoke ok`
# means the DOM actually changed; `e2e ok` means every step passed.
#
# The runtime path in `examples/counter/runai.json` is `../../packages/runai/runtime.js`:
# relative to the `runai.json` that names it, so the committed example works on any
# checkout, at any absolute path, with no environment variable.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/examples/counter"

# `check` first, and as JSON: a build would report the same diagnostics, but the
# clean verdict has to be provable before anything is written. `[]` is the only
# accepted output — an exit code alone would pass on a tree with warnings.
out="$(mktemp)"
trap 'rm -f "$out"' EXIT
cargo run --quiet --manifest-path "$root/Cargo.toml" -p runai_cli -- check --json > "$out"
test "$(cat "$out")" = "[]" || { cat "$out"; exit 1; }

cargo run --quiet --manifest-path "$root/Cargo.toml" -p runai_cli -- build --out dist

# The published tree, in the shape the build contract fixes.
test -f dist/index.html
test -f dist/about/index.html
test -f dist/assets/index.js
test -f dist/assets/runtime.js

# The island: the container the module mounts into, and the module tag that loads
# it. Both are asserted because a page with the script and no container mounts
# nothing, and a page with the container and no script never runs.
grep -q '<div id="app"></div>' dist/index.html
grep -q '<script type="module" src="/assets/index.js"></script>' dist/index.html

# The static route ships 0 bytes of JavaScript. A bare `! grep` is exempt from
# `set -e`, so it would never fail the script; the check is written as an explicit
# `if` for that reason.
if grep -q '<script' dist/about/index.html; then
  echo "error: the static route ships JavaScript: $(grep -o '<script[^>]*>' dist/about/index.html | head -1)"
  exit 1
fi

# The built island, mounted and clicked under happy-dom. `happy-dom` is a
# devDependency of `packages/runai`, so the script runs from there to resolve it —
# the example adds no dependency of its own.
(cd "$root/packages/runai" && bun test/smoke-island.mjs "$root/examples/counter/dist")

echo "e2e ok"
