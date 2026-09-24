import { afterEach, describe, expect, it, vi } from 'vitest'

import { magnetLink, playlistUrl, playUrl, streamUrl } from './links'
import { addTorrent, listTorrents, torrentFiles, uploadTorrent } from './torrents'
import { TorrentStat, type Torrent } from './types'

function respond(body: string, status = 200) {
  const fetch = vi.fn().mockResolvedValue(new Response(body, { status }))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

function sent(fetch: ReturnType<typeof respond>) {
  const [path, init] = fetch.mock.calls[0] as [string, RequestInit]
  return { path, init, json: typeof init.body === 'string' ? JSON.parse(init.body) : init.body }
}

afterEach(() => vi.unstubAllGlobals())

const torrent: Torrent = {
  hash: 'd272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d',
  title: 'Медиа коллекция',
  category: '',
  poster: '',
  timestamp: 1790219490,
  stat: TorrentStat.InDb,
  stat_string: 'Torrent in db',
  data: '{"TorrServer":{"Files":[{"id":1,"path":"Медиа коллекция/01 Пример/Фильм.mkv","length":524288}]}}',
}

describe('the torrents API', () => {
  it('lists torrents, and an empty answer is an empty list', async () => {
    const fetch = respond('null')
    await expect(listTorrents()).resolves.toEqual([])
    expect(sent(fetch).json).toEqual({ action: 'list' })
  })

  it('adds a link saved to the catalog by default', async () => {
    const fetch = respond(JSON.stringify(torrent))
    await addTorrent('magnet:?xt=urn:btih:abc', { title: 'Фильм' })
    expect(sent(fetch).json).toEqual({
      action: 'add',
      link: 'magnet:?xt=urn:btih:abc',
      title: 'Фильм',
      poster: '',
      category: '',
      data: '',
      save_to_db: true,
    })
  })

  it('uploads a .torrent as multipart with the reference field names', async () => {
    const fetch = respond(JSON.stringify(torrent))
    await uploadTorrent(new File(['d4:infoe'], 'movie.torrent'), { category: 'movie' })
    const { path, init } = sent(fetch)
    expect(path).toBe('/torrent/upload')
    const form = init.body as FormData
    expect((form.get('file') as File).name).toBe('movie.torrent')
    expect(form.get('save')).toBe('true')
    expect(form.get('category')).toBe('movie')
    expect(form.has('title')).toBe(false)
  })

  it('reads the files of a catalog entry from its data', () => {
    expect(torrentFiles(torrent)).toEqual([{ id: 1, path: 'Медиа коллекция/01 Пример/Фильм.mkv', length: 524288 }])
    expect(torrentFiles({ ...torrent, data: 'not json' })).toEqual([])
  })
})

describe('links', () => {
  const file = { id: 1, path: 'Медиа коллекция/01 Пример/Фильм.mkv', length: 524288 }

  it('point players at the server routes', () => {
    expect(playUrl(torrent.hash, file.id)).toBe(`/play/${torrent.hash}/1`)
    expect(streamUrl('http://nas:8090', torrent.hash, file)).toBe(
      `http://nas:8090/stream/%D0%A4%D0%B8%D0%BB%D1%8C%D0%BC.mkv?link=${torrent.hash}&index=1&play`,
    )
    expect(playlistUrl('http://nas:8090', torrent, true)).toBe(
      `http://nas:8090/stream/%D0%9C%D0%B5%D0%B4%D0%B8%D0%B0%20%D0%BA%D0%BE%D0%BB%D0%BB%D0%B5%D0%BA%D1%86%D0%B8%D1%8F.m3u?link=${torrent.hash}&m3u&fromlast`,
    )
    expect(magnetLink(torrent)).toBe(
      `magnet:?xt=urn:btih:${torrent.hash}&dn=%D0%9C%D0%B5%D0%B4%D0%B8%D0%B0%20%D0%BA%D0%BE%D0%BB%D0%BB%D0%B5%D0%BA%D1%86%D0%B8%D1%8F`,
    )
  })
})
