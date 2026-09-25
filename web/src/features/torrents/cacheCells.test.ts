import { describe, expect, it } from 'vitest'

import type { CacheState } from '../../api/cache'
import { cacheCells } from './cacheCells'

const piece = (Id: number, Size: number, Completed: boolean) => ({ Id, Length: 100, Size, Completed, Priority: 0 })

describe('cache map', () => {
  const cache: CacheState = {
    Hash: 'h',
    Capacity: 1000,
    Filled: 250,
    PiecesLength: 100,
    PiecesCount: 6,
    Pieces: { 0: piece(0, 100, true), 1: piece(1, 50, false), 4: piece(4, 100, true) },
    Readers: [{ Start: 3, End: 4, Reader: 3 }],
  }

  it('show each piece while they fit', () => {
    expect(cacheCells(cache).map((cell) => cell.state)).toEqual(['cached', 'partial', 'empty', 'reading', 'cached', 'empty'])
  })

  it('fold pieces into fewer cells', () => {
    const cells = cacheCells(cache, 3)
    expect(cells.map(({ from, to }) => [from, to])).toEqual([[0, 1], [2, 3], [4, 5]])
    expect(cells.map((cell) => cell.state)).toEqual(['partial', 'reading', 'partial'])
    expect(cells[0]?.filled).toBe(0.75)
  })

  it('draw nothing without pieces', () => {
    expect(cacheCells({ ...cache, PiecesCount: 0 })).toEqual([])
  })
})
