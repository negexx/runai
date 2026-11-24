---
name: RA011_BAD_ROUTE_EXPORT
description: A page file's default export is missing, is not a component, or cannot render the route.
---

## What it means

A page file's default export is missing, is not a function this milestone can analyze, takes parameters a route cannot be given, or does not return JSX from the body. The router renders the default export as the route, and only a direct `return <…/>` is recognized, because resolving what a returned value renders needs type information this compiler does not have yet.

## Why it is an error

The router renders the default export as the route, and it has no other source. Three shapes produce this code, and the `message` says which one the file has: there is no default export at all; a default export exists but is not a function this milestone can analyze (a class, a literal, a call, or an identifier that cannot be resolved in this file); or it is a function whose body does not `return <…/>` directly, or it takes more than one parameter. Only a direct `return` is recognized, because resolving what a returned variable or conditional renders needs type information this compiler does not have yet. An export the router cannot call, cannot fill, or cannot get markup from leaves the route with nothing to render. The canonical fix is one sentence for every site, so the per-file half is in the `message`: it names the existing declaration and, for a named export, quotes the signature line to edit.

## Fix

Add a default export if it is missing. Replace the existing default export if it is not a component, rather than declaring a second one. Either way it must take at most one annotated `props` parameter and return JSX directly from its body.
