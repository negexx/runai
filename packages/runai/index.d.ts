/** Reads with no arguments, writes with one: `count()` / `count(next)`. */
export type Accessor<T> = {
  (): T
  (next: T): void
}

export declare function signal<T>(initial: T): Accessor<T>

/** Read-only. Recomputes only when read after a dependency changed. */
export declare function derived<T>(fn: () => T): () => T

/** Runs now and on every change to what it read. Returns a disposer. */
export declare function effect(fn: () => void): () => void

/** Disposal scope: effects and owner scopes created inside are disposed together. */
export declare function owner(fn: () => void): () => void

/**
 * Codegen internals. Emitted by the compiler, not authored by hand.
 * A function argument is a reactive thunk, re-run on dependency change;
 * anything else is a constant. Each returns a disposer.
 */
export declare function _setText<T>(node: Node, value: T | (() => T)): () => void

/**
 * `false`/`null`/`undefined` remove the attribute; `true` sets it bare
 * (`name=""`); anything else is stringified, so `0` and `""` are set.
 */
export declare function _setAttr<T>(
  node: Element,
  name: string,
  value: T | (() => T),
): () => void

export declare function _on(
  node: Element,
  name: string,
  handler: (event: Event) => void,
): () => void

// --- components ------------------------------------------------------------

/**
 * Parsed markup, rendered by cloning its content into the document.
 * Never assign `innerHTML` on a live node: clone the fragment instead.
 */
export interface Template {
  readonly content: DocumentFragment
  /** A fresh, independent fragment each call. */
  clone(): DocumentFragment
}

export declare function template(html: string): HTMLTemplateElement & Template

/**
 * Keyed list. `children` runs once per key; reordering moves the existing
 * nodes instead of recreating them. A removed key's scope is disposed.
 * Returns a fragment ending with an internal comment anchor.
 */
export declare function For<T>(props: {
  each: () => T[]
  by: (item: T) => string | number
  children: (item: () => T) => Node
}): Node

/** Conditional branch. `fallback` is optional; a hidden branch is not disposed. */
export declare function Show(props: {
  when: () => boolean
  fallback?: Node
  children: Node
}): Node

export type Result<T> =
  | { status: "pending" }
  | { status: "error"; error: unknown }
  | { status: "success"; data: T }

/**
 * The only way to render an async value: takes the accessor **uncalled**,
 * requires all three branches, and renders `pending` synchronously.
 */
export declare function When<T>(props: {
  value: () => Result<T>
  pending: Node
  error: (error: unknown) => Node
  children: (data: T) => Node
}): Node

/**
 * Runs `fn` and re-runs it when a signal it reads changes. A one-argument `fn`
 * receives `{ signal: AbortSignal }`, aborted when it re-runs or is disposed.
 * A synchronous throw or a rejection lands in the `error` state, never a
 * rejection out of this call.
 */
export declare function resource<T>(
  fn: (ctx: { signal: AbortSignal }) => Promise<T> | T,
): Accessor<Result<T>>

/**
 * Island entry point: renders `component` into `root` under one owner scope.
 * The returned disposer tears the scope down and removes the rendered nodes.
 */
export declare function mount(component: (props: Record<string, never>) => Node, root: Element): () => void
