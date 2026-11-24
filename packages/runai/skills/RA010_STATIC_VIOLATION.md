---
name: RA010_STATIC_VIOLATION
description: A `"use static"` route uses a signal, effect, resource, write, or handler it cannot run.
---

## What it means

A `"use static"` route uses a signal, a derived value, an effect, a resource, a write, or an event handler. Static routes ship zero bytes of JavaScript, so there is no runtime on the page to run the feature; it would silently do nothing.

## Why it is an error

Static routes ship zero bytes of JavaScript. There is no runtime on the page to run a signal or attach a listener, so the feature would silently do nothing.

## Fix

Change this file's "use static" directive to "use island" to ship JavaScript for this route.
