import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import {
  type AddOptions,
  addTorrent,
  dropTorrent,
  getTorrent,
  listTorrents,
  listViewed,
  removeTorrent,
  uploadTorrent,
} from '../../api/torrents'
import { isLive } from './status'

export const torrentKeys = {
  all: ['torrents'] as const,
  one: (hash: string) => ['torrents', hash] as const,
  viewed: (hash: string) => ['viewed', hash] as const,
}

/** The list, refreshed every two seconds while anything is loaded. */
export function useTorrents() {
  return useQuery({
    queryKey: torrentKeys.all,
    queryFn: listTorrents,
    refetchInterval: (query) => (query.state.data?.some(isLive) ? 2_000 : 10_000),
  })
}

/** One torrent, every second: its statistics move while it plays. */
export function useTorrent(hash: string) {
  return useQuery({ queryKey: torrentKeys.one(hash), queryFn: () => getTorrent(hash), refetchInterval: 1_000 })
}

export function useViewed(hash: string) {
  return useQuery({ queryKey: torrentKeys.viewed(hash), queryFn: () => listViewed(hash) })
}

function useInvalidating<T>(action: (value: T) => Promise<unknown>) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: action,
    onSettled: () => client.invalidateQueries({ queryKey: torrentKeys.all }),
  })
}

export function useAddLink() {
  return useInvalidating(({ link, options }: { link: string; options: AddOptions }) => addTorrent(link, options))
}

export function useUpload() {
  return useInvalidating(async ({ files, options }: { files: File[]; options: AddOptions }) => {
    for (const file of files) await uploadTorrent(file, options)
  })
}

export function useRemove() {
  return useInvalidating(removeTorrent)
}

export function useDrop() {
  return useInvalidating(dropTorrent)
}
