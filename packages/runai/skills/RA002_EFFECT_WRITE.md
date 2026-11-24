---
name: RA002_EFFECT_WRITE
description: A signal written inside an `effect` body is a side effect of that run, not a derived value.
---

## What it means

A signal is written inside an `effect(...)` argument. An effect re-runs for the signals it reads, and a write in its body executes as part of that run, so the write is a side effect of the effect rather than a derived value. When the effect also reads the signal it writes, that write causes the next run in turn.

## Why it is an error

An effect re-runs for the signals it reads, and a write in its body executes as part of that run. So the write is a side effect of the effect, not a derived value: when the effect also reads the signal it writes, each run schedules the next one, which is a loop. An effect that reads nothing cannot re-run at all, so the write there is a one-shot side effect — still not a derived value, and still the wrong place for it.

## Fix

Derive the value instead of writing it, or move the write into the event handler that causes the change, and let the effect only read what it needs.
