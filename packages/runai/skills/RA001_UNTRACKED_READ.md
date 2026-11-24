---
name: RA001_UNTRACKED_READ
description: A signal read in the component body outside any tracking scope freezes at the first render.
---

## What it means

A signal is read directly in the component body, outside any tracking scope. The read runs once and never again: no effect owns it, so nothing re-runs when the signal changes, and the value on screen is the value at first render. A read whose nearest enclosing function is *not* the component body — an event handler, or any other nested callback such as `setTimeout(() => n())` — is not this rule: it runs when that callback runs.

## Why it is an error

A read there runs once and never again: no effect owns it, so nothing re-runs when the signal changes. The value on screen is the value at first render. This is why `RA001` only fires when the read's *nearest enclosing function* is the component body itself: a read inside an event handler (`onClick={() => count()}`) or any other nested callback (`setTimeout(() => count(), 10)`) runs when that callback runs, so it sees the current value and nothing is frozen. Those reads are correct and are never flagged. A read inside `derived(...)`, `effect(...)`, `resource(...)`, or a JSX expression slot is likewise owned by that tracking scope, not by the render.

## Fix

Wrap the read in a tracking scope — `derived(() => count() * 2)`, an `effect(...)`, a `resource(...)`, or a JSX slot — or call it inside an event handler.
