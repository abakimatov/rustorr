import { describe, expect, it } from 'vitest'

import { href, parseRoute } from './router'

describe('routes', () => {
  it('round-trip through the hash', () => {
    for (const route of [
      { name: 'torrents' },
      { name: 'torrent', hash: 'd272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d' },
      { name: 'torrent', hash: 'd272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d', file: 2 },
      { name: 'search' },
      { name: 'settings', section: 'cache' },
    ] as const) {
      expect(parseRoute(href(route))).toEqual(route.name === 'settings' ? route : { ...route })
    }
  })

  it('fall back to the torrent list', () => {
    expect(parseRoute('')).toEqual({ name: 'torrents' })
    expect(parseRoute('#/nowhere')).toEqual({ name: 'torrents' })
    expect(parseRoute('#/torrent')).toEqual({ name: 'torrents' })
    expect(parseRoute('#/torrent/abc/x')).toEqual({ name: 'torrent', hash: 'abc' })
  })
})
