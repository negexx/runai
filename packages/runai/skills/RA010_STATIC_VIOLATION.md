---
name: RA010_STATIC_VIOLATION
description: A `"use static"` route uses a signal, effect, resource, write, handler, or script it cannot ship.
---

## What it means

A `"use static"` route ships a construct that needs JavaScript a prerendered page does not have: a signal, a derived value, an effect, a resource, a write, a runtime primitive, or code the markup itself carries — an inline `on…` handler, a literal `<script>` element. The page ships zero bytes of JavaScript, so a runtime feature would silently do nothing, while an inline handler or a script element is code the served page would run — neither can be on a page that makes the 0 KB claim. An `on…` attribute that is not a handler HTML defines (`once`, `onwards`) is refused as well: the rule is a name prefix, and such an attribute is either dead or inline JavaScript, which a static route has no runtime for.

## Why it is an error

Static routes ship zero bytes of JavaScript, and this rule is what enforces that claim rather than observing it. Three costs, and the `message` says which one a site has. A runtime feature — a signal, an effect, a resource, a write — would silently do nothing, because there is no runtime on the page to run it. Code the markup carries is worse: an `on…` attribute the emitted HTML keeps under a name HTML defines as a handler, and a literal `<script>` element, are JavaScript the served HTML would execute, so the page would ship exactly what it promises not to. And because the handler rule is a name prefix rather than an event list — a list cannot be complete and nothing fails when it drifts — an `on…` attribute the HTML keeps under a name that is *not* a handler (`once`, `onwards`) is refused too: it is either dead or inline JavaScript, and a static route has no runtime for either. All three are refused for the same reason: the directive is the decision that controls the 0 KB guarantee, and a page that needs code is an island.

## Fix

Change this file's "use static" directive to "use island" to ship JavaScript for this route.
