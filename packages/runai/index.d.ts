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
