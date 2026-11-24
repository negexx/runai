---
name: RA002_EFFECT_WRITE
description: A signal written inside an `effect` body makes the write its own dependency.
---

## What it means

A signal is written inside an `effect(...)` argument. An effect re-runs when the signals it reads change, so writing one makes that write a dependency of the run that performed it — a loop.

## Why it is an error

An effect re-runs when the signals it reads change. Writing a signal it reads makes that write a new dependency of the run that performed it, which is a loop.

## Fix

Derive the value instead of writing it, or move the write into the event handler that causes the change, and let the effect only read what it needs.
