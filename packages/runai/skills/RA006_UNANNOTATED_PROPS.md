---
name: RA006_UNANNOTATED_PROPS
description: A component's props parameter has no explicit type, or there is no parameter at all.
---

## What it means

A component's props parameter is missing an explicit type annotation, or the component takes no parameter at all. Props are the only way data enters a component, so their type is the written answer to "where does this value come from?", and a propless component has to write that answer down too.

## Why it is an error

An unannotated parameter leaves the answer unstated, and a propless component leaves it to prose. The next reader — human or agent — has to guess what may be passed. The canonical fix is one sentence for every site, so the per-file half lives in the diagnostic's `message`: it quotes the exact signature line to write, in the declaration form the file already uses, naming the binding that exists rather than a synthesized `function Page(…)`.

## Fix

Give the component exactly one typed parameter. With no props, write (props: Record<never, never>).
