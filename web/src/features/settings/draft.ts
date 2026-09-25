/** Top-level fields whose values differ, compared by their JSON form. */
export function changedKeys<T extends object>(saved: T, draft: T): (keyof T)[] {
  return (Object.keys(draft) as (keyof T)[]).filter(
    (key) => JSON.stringify(saved[key]) !== JSON.stringify(draft[key]),
  )
}

export const MIB = 1024 * 1024

/** Lines of a textarea list, trimmed, without blanks. */
export function lines(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
}
