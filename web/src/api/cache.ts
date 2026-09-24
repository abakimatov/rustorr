import { postJson } from './http'

export interface CachePiece {
  Id: number
  Length: number
  /** Bytes of the piece in the cache. */
  Size: number
  Completed: boolean
  Priority: number
}

/** Pieces `[Start, End)` a reader is working through. */
export interface CacheReader {
  Start: number
  End: number
  Reader: number
}

export interface CacheState {
  Hash: string
  Capacity: number
  Filled: number
  PiecesLength: number
  PiecesCount: number
  Pieces: Record<string, CachePiece> | null
  Readers: CacheReader[] | null
}

/** `404` while the torrent is not loaded. */
export function getCache(hash: string): Promise<CacheState> {
  return postJson<CacheState>('/cache', { action: 'get', hash })
}
