---
name: RA012_MISSING_MODE
description: The file opens without a `"use static"` or `"use island"` directive.
---

## What it means

The file does not open with a `"use static"` or `"use island"` directive. The directive decides whether the route ships JavaScript, and inferring it would hide the decision that controls the 0 KB guarantee, so it is never inferred.

## Why it is an error

The directive decides whether the route ships JavaScript. Inferring it would hide the decision that controls the 0 KB guarantee.

## Fix

Add "use static" (prerendered to HTML, no JavaScript) or "use island" (hydrated on boot) as the first statement of the file. Single and double quotes are both accepted; escapes such as "use\x20static" are not.
