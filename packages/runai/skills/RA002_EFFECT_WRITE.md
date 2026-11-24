---
name: RA002_EFFECT_WRITE
description: A signal written inside an `effect` body is a side effect of that run, not a derived value.
---

## What it means

A signal is written inside an `effect(...)` argument. An effect re-runs for the signals it reads, and a write in its body executes as part of that run, so the write is a side effect of the effect rather than a derived value. When the effect also reads the signal it writes, the write during a run queues that same effect, and asking it to run while it is already running does not loop: the runtime throws `effect cycle: an effect wrote a signal it reads` (and a wave limit throws when effects keep invalidating each other). A write of the value the signal already holds never notifies anyone — the setter bails out first — so it neither re-runs nor throws.

## Why it is an error

An effect re-runs for the signals it reads, and a write in its body executes as part of that run. So the write is a side effect of the effect, not a derived value. When the effect also reads the signal it writes, the write queues the effect that is running, and the runtime does not loop: it throws `effect cycle: an effect wrote a signal it reads`, and after 100 waves of effects invalidating each other it throws the cycle error instead of spinning. Writing back the value the signal already holds is not even a notification — the setter bails out before notifying — so it never re-runs the effect and cannot throw. An effect that reads nothing cannot re-run at all, so the write there is a one-shot side effect — still not a derived value, and still the wrong place for it.

## Fix

Derive the value instead of writing it, or move the write into the event handler that causes the change, and let the effect only read what it needs.
