import { HttpError, request } from './http'

/** One result, as MatriX.145 writes `TorrentDetails`. */
export interface SearchResult {
  Title: string
  Name: string
  Names: string[] | null
  Categories: string
  /** As the source wrote it, e.g. `4.37 GB`. */
  Size: string
  /** RFC 3339. */
  CreateDate: string
  Tracker: string
  Link: string
  Year: number
  Peer: number
  Seed: number
  Magnet: string
  Hash: string
  IMDBID: string
}

export type SearchSource = 'rutor' | 'torznab'

/** The source is switched off in the settings: the server answers `400 []`. */
export class SearchDisabled extends Error {}

export async function search(source: SearchSource, query: string, indexer = -1): Promise<SearchResult[]> {
  const params = new URLSearchParams({ query })
  if (source === 'torznab' && indexer >= 0) params.set('index', String(indexer))
  const path = source === 'rutor' ? '/search' : '/torznab/search'
  try {
    return ((await (await request(`${path}?${params}`)).json()) as SearchResult[] | null) ?? []
  } catch (error) {
    if (error instanceof HttpError && error.status === 400) throw new SearchDisabled(source)
    throw error
  }
}
