---
name: RA007_AMBIGUOUS_JSX
description: The parser rejected the file, or a line break sits between two inline elements and deletes a visible space.
---

## What it means

Either the parser rejected this file before it could be compiled, or a line break sits between two inline elements. Nothing downstream — lowering, analysis, codegen — can run on a tree the parser failed to produce, so the parser's own message and location are reported instead of a guess at what was meant. A line break between two inline elements is the other case: JSX deletes whitespace-only text that contains a newline, so the space the author saw in the editor is not the space the page shows.

## Why it is an error

Nothing downstream — lowering, analysis, codegen — can run on a file with a syntax error. The diagnostic carries the parser's own message and location so the repair lands where the error actually is. The same code covers a line break between two inline elements, because that is the other way the file's visible text and its rendered text disagree: JSX deletes whitespace-only text containing a newline.

## Fix

Fix the syntax error the parser reported in `message`. In route files the usual causes are an unclosed element, an unterminated string inside an attribute, or adjacent sibling elements — add the missing `>` or closing element, close the attribute string, or wrap the siblings in one parent element. When `message` names a line break between inline elements instead, the repair is the one it carries. Put both on one line, or write {" "} where you want the space.
