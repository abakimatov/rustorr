import type { FileStat, Torrent } from './types'

/** The last path segment, as players name the stream. */
export function fileName(path: string): string {
  return path.split('/').pop() ?? path
}

/** Raw bytes of one file, for the browser's `<video>`. */
export function playUrl(hash: string, file: FileStat): string {
  return `/play/${hash}/${file.id}`
}

/** An absolute link for external players (`/stream/<name>?…&play`). */
export function streamUrl(origin: string, hash: string, file: FileStat): string {
  return `${origin}/stream/${encodeURIComponent(fileName(file.path))}?link=${hash}&index=${file.id}&play`
}

/** The torrent's M3U playlist; `fromLast` starts at the last viewed file. */
export function playlistUrl(origin: string, torrent: Torrent, fromLast = false): string {
  const name = encodeURIComponent(`${torrent.title || torrent.name || torrent.hash}.m3u`)
  return `${origin}/stream/${name}?link=${torrent.hash}&m3u${fromLast ? '&fromlast' : ''}`
}

export function allPlaylistUrl(origin: string): string {
  return `${origin}/playlistall/all.m3u`
}

export function magnetLink(torrent: Torrent): string {
  const name = torrent.title || torrent.name
  return `magnet:?xt=urn:btih:${torrent.hash}${name ? `&dn=${encodeURIComponent(name)}` : ''}`
}
