import type { CacheState } from '../../api/cache'

export type CellState = 'empty' | 'partial' | 'cached' | 'reading'

export interface Cell {
  state: CellState
  /** First and last piece the cell stands for. */
  from: number
  to: number
  /** Share of the cell's bytes in the cache, 0–1. */
  filled: number
}

/** Pieces folded into at most `maxCells` cells, so a torrent of thousands of
 * pieces still draws a readable map. A cell a reader is inside is `reading`. */
export function cacheCells(cache: CacheState, maxCells = 600): Cell[] {
  const count = cache.PiecesCount
  if (count <= 0) return []
  const perCell = Math.ceil(count / maxCells)
  const cells: Cell[] = []
  const pieces = cache.Pieces ?? {}
  const readers = cache.Readers ?? []
  for (let from = 0; from < count; from += perCell) {
    const to = Math.min(count, from + perCell) - 1
    let filled = 0
    for (let id = from; id <= to; id++) {
      const piece = pieces[id]
      if (piece) filled += piece.Completed ? 1 : piece.Length > 0 ? Math.min(1, piece.Size / piece.Length) : 0
    }
    filled /= to - from + 1
    const reading = readers.some((reader) => reader.Start <= to && reader.End > from)
    const state: CellState = reading ? 'reading' : filled >= 0.999 ? 'cached' : filled > 0 ? 'partial' : 'empty'
    cells.push({ state, from, to, filled })
  }
  return cells
}
