export declare function signal<T>(initial: T): {
  (): T
  (next: T): T
}
