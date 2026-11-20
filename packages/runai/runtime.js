export function signal(initial) {
  let value = initial
  return (...args) => {
    if (args.length === 0) return value
    value = args[0]
    return value
  }
}
