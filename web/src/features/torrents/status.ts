import { TorrentStat, type Torrent } from '../../api/types'

const keys: Record<TorrentStat, string> = {
  [TorrentStat.Added]: 'status.added',
  [TorrentStat.GettingInfo]: 'status.gettingInfo',
  [TorrentStat.Preload]: 'status.preload',
  [TorrentStat.Working]: 'status.working',
  [TorrentStat.Closed]: 'status.closed',
  [TorrentStat.InDb]: 'status.inDb',
}

export function statusKey(torrent: Torrent): string {
  return keys[torrent.stat] ?? 'status.inDb'
}

export function statusTone(torrent: Torrent): 'ok' | 'warn' | 'idle' {
  switch (torrent.stat) {
    case TorrentStat.Working:
    case TorrentStat.Preload:
      return 'ok'
    case TorrentStat.Added:
    case TorrentStat.GettingInfo:
      return 'warn'
    default:
      return 'idle'
  }
}

/** Loaded into memory, as opposed to only kept in the catalog. */
export function isLive(torrent: Torrent): boolean {
  return torrent.stat !== TorrentStat.InDb && torrent.stat !== TorrentStat.Closed
}

export const categories = ['movie', 'tv', 'music', 'other'] as const

/** Initials for the poster placeholder. */
export function initials(title: string): string {
  const words = title.replace(/[^\p{L}\p{N}\s]/gu, ' ').split(/\s+/).filter(Boolean)
  return words.slice(0, 3).map((word) => word[0]?.toUpperCase()).join('') || '?'
}
