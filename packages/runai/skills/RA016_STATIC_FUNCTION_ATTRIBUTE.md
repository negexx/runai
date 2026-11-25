---
name: RA016_STATIC_FUNCTION_ATTRIBUTE
description: A `"use static"` route gives an attribute a value the build cannot evaluate, so a prerendered page and a hydrated island would render different attributes.
---

## What it means

A `"use static"` route gives an attribute a value that cannot be evaluated at build time. A static page renders its markup to one HTML string at build time and ships no JavaScript, so every attribute value is either written as text or interpolated as a value already known then. A value that is a function is not text: a hydrated island calls it, so the same source would render the function in a prerendered page and its result in a hydrated one.

## Why it is an error

A `"use static"` route ships zero bytes of JavaScript: `render()` builds one HTML string at build time and nothing runs on the page. An attribute value is therefore either literal text or a value already known at build time, and a function is neither. A hydrated island's `_setAttr` calls a function value and uses the result, so the same source would render two different attributes in the two modes — the function's own source text in a prerendered page, its return value in a hydrated one — with nothing reported. Refusing the static form is the only honest answer: the build cannot call the function, because the module it emits has no runtime to call it with, and evaluating authored code at build time would run it with no DOM anyway. The repair is to write the value as text the build can see, or to change the directive when the value genuinely must be computed in the browser.

## Fix

Write the attribute value as a string literal (`class="a"`), a template literal with no substitutions, or a constant this file defines; a static route evaluates its markup at build time and cannot call a function. If the value must be computed in the browser, change the directive to "use island".
