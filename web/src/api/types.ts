/** `TorrentStatus.Stat`: where a torrent is in its life. */
export const TorrentStat = {
  Added: 0,
  GettingInfo: 1,
  Preload: 2,
  Working: 3,
  Closed: 4,
  InDb: 5,
} as const
export type TorrentStat = (typeof TorrentStat)[keyof typeof TorrentStat]

export interface FileStat {
  id: number
  path: string
  length: number
}

/** A torrent as `/torrents` reports it; the statistics are present only
 * while the torrent is loaded. */
export interface Torrent {
  hash: string
  title: string
  category: string
  poster: string
  data?: string
  timestamp: number
  name?: string
  torrs_hash?: string
  stat: TorrentStat
  stat_string: string
  torrent_size?: number
  loaded_size?: number
  preloaded_bytes?: number
  preload_size?: number
  download_speed?: number
  upload_speed?: number
  total_peers?: number
  pending_peers?: number
  active_peers?: number
  connected_seeders?: number
  half_open_peers?: number
  file_stats?: FileStat[]
}

export interface Viewed {
  hash: string
  file_index: number
}
