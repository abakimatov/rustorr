import { postJson, request } from './http'
import type { FileStat, Torrent, Viewed } from './types'

export interface AddOptions {
  title?: string
  poster?: string
  category?: string
  data?: string
  /** Keep the torrent in the catalog across restarts. */
  save?: boolean
}

export async function listTorrents(): Promise<Torrent[]> {
  return (await postJson<Torrent[] | null>('/torrents', { action: 'list' })) ?? []
}

export function getTorrent(hash: string): Promise<Torrent> {
  return postJson<Torrent>('/torrents', { action: 'get', hash })
}

/** A magnet link, an `http(s)` link to a `.torrent`, an info hash or a
 * `torrs://` link. */
export function addTorrent(link: string, options: AddOptions = {}): Promise<Torrent> {
  return postJson<Torrent>('/torrents', {
    action: 'add',
    link,
    title: options.title ?? '',
    poster: options.poster ?? '',
    category: options.category ?? '',
    data: options.data ?? '',
    save_to_db: options.save ?? true,
  })
}

/** `.torrent` files through `/torrent/upload`, one request each. */
export async function uploadTorrent(file: File, options: AddOptions = {}): Promise<Torrent> {
  const form = new FormData()
  form.append('file', file, file.name)
  if (options.save ?? true) form.append('save', 'true')
  for (const key of ['title', 'poster', 'category', 'data'] as const) {
    const value = options[key]
    if (value) form.append(key, value)
  }
  const response = await request('/torrent/upload', { method: 'POST', body: form })
  return (await response.json()) as Torrent
}

export function updateTorrent(
  hash: string,
  fields: { title: string; poster: string; category: string },
): Promise<void> {
  return postJson('/torrents', { action: 'set', hash, ...fields, data: '' })
}

/** Removes it from the catalog and memory. */
export function removeTorrent(hash: string): Promise<void> {
  return postJson('/torrents', { action: 'rem', hash })
}

/** Unloads it from memory; a saved torrent stays in the catalog. */
export function dropTorrent(hash: string): Promise<void> {
  return postJson('/torrents', { action: 'drop', hash })
}

export async function listViewed(hash?: string): Promise<Viewed[]> {
  return (await postJson<Viewed[] | null>('/viewed', { action: 'list', hash: hash ?? '' })) ?? []
}

export function setViewed(hash: string, fileIndex: number): Promise<void> {
  return postJson('/viewed', { action: 'set', hash, file_index: fileIndex })
}

export function clearViewed(hash: string, fileIndex = -1): Promise<void> {
  return postJson('/viewed', { action: 'rem', hash, file_index: fileIndex })
}

/** The torrent's files: the live list, else the one kept in `data` for a
 * catalog entry that is not loaded. */
export function torrentFiles(torrent: Torrent): FileStat[] {
  if (torrent.file_stats?.length) return torrent.file_stats
  if (!torrent.data) return []
  try {
    const parsed = JSON.parse(torrent.data) as { TorrServer?: { Files?: FileStat[] } }
    return parsed.TorrServer?.Files ?? []
  } catch {
    return []
  }
}
