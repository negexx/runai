---
name: RA009_IMPLICIT_EFFECT
description: A signal is written during render, which is a side effect in rendering.
---

## What it means

A signal is written during render: in the component body or in a JSX expression container. The component body runs once, so this write cannot re-run the component. Rendering must not have side effects.

## Why it is an error

Rendering must not have side effects. The component body runs once, so this write cannot re-run the component.

## Fix

Derive the value instead of writing it, or move the write into the event handler that causes the change. The component body runs once, so this write cannot re-run the component.
