---
name: RA015_EXCESSIVE_NESTING
description: A JSX tree nests deeper than the compiler can walk, which would overflow the stack and abort the process.
---

## What it means

A JSX tree nests deeper than the compiler can walk. Every pass over the markup — parsing, the whitespace and refusal checks, lowering, and writing the template — visits one element per level of recursion, so a tree past a certain depth exhausts the stack. A stack overflow aborts the process rather than returning an error, which would leave the agent with no diagnostic and no file position. The file is therefore refused at the depth limit instead, which is a deliberate ceiling on the input rather than a claim about the markup's meaning.

## Why it is an error

Every pass over the markup is recursive descent, one stack frame per element level: the parser builds the tree, the whitespace and refusal checks walk it, lowering turns it into the IR, and the emitters write the template. The stack is finite, so a tree deep enough exhausts it, and a stack overflow **aborts the process** — it is not a panic a `Result` can carry, so there is no diagnostic and no file position at all. The compiler would die with `SIGABRT` on input an agent wrote by accident, for example a generator that emitted one wrapper too many. The depth ceiling turns that into an ordinary refusal: the file is reported at the element that crosses it, with a position the agent can edit. The limit is 512 levels, measured against the smallest stack this crate runs on (a 2 MiB test thread, where the parser alone aborts between 1000 and 1100 levels); real markup nests far shallower, so the ceiling only refuses a tree that is already pathological.

## Fix

Reduce the nesting to at most 512 levels by closing elements earlier: move a deeply nested subtree into a sibling element, or flatten repeated wrappers that carry no attribute. Do not split the markup into another `.tsx` module — this milestone has no bundler and no component inlining, so importing one is refused with `RA014`.
