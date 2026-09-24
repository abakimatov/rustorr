import { afterEach, describe, expect, it, vi } from 'vitest'

import { getGstSettings, resetSettings, saveGstSettings, saveSettings, saveStorage, saveWaf, testTorznab, type BtSettings, type GstConfig } from './settings'

function respond(body: string, status = 200) {
  const fetch = vi.fn().mockResolvedValue(new Response(body, { status }))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

function sent(fetch: ReturnType<typeof respond>) {
  const [path, init] = fetch.mock.calls[0] as [string, RequestInit]
  return { path, method: init.method ?? 'GET', json: init.body ? JSON.parse(init.body as string) : undefined }
}

afterEach(() => vi.unstubAllGlobals())

describe('settings api', () => {
  it('sends the whole BTSets on save and nothing on reset', async () => {
    let fetch = respond('')
    const sets = { CacheSize: 64 << 20, TrackTimecode: true } as BtSettings
    await saveSettings(sets)
    expect(sent(fetch)).toEqual({ path: '/settings', method: 'POST', json: { action: 'set', sets } })

    fetch = respond('')
    await resetSettings()
    expect(sent(fetch).json).toEqual({ action: 'def' })
  })

  it('use the module endpoints as MatriX does', async () => {
    let fetch = respond('{"whitelist":"","blacklist":"","referers":"","ip_enabled":false,"referer_enabled":false,"read_only":false,"warnings":[]}')
    await saveWaf({ whitelist: '10.0.0.0/8', blacklist: '', referers: '' })
    expect(sent(fetch)).toEqual({ path: '/waf', method: 'POST', json: { whitelist: '10.0.0.0/8', blacklist: '', referers: '' } })

    fetch = respond('{"success":false,"error":"api error: Invalid API Key"}')
    expect(await testTorznab('http://indexer:9117', 'k')).toEqual({ success: false, error: 'api error: Invalid API Key' })
    expect(sent(fetch).json).toEqual({ host: 'http://indexer:9117', key: 'k' })

    fetch = respond('{"status":"ok"}')
    await saveStorage({ settings: 'json', viewed: 'bbolt' })
    expect(sent(fetch)).toEqual({ path: '/storage/settings', method: 'POST', json: { settings: 'json', viewed: 'bbolt' } })

    fetch = respond('{"status":"ok"}')
    const config = { Source: 'play', SegmentSeconds: 6 } as GstConfig
    await saveGstSettings(config)
    expect(sent(fetch).json).toEqual({ action: 'set', config })
  })

  it('reads a build without GStreamer', async () => {
    respond('{"built_in":false}')
    expect(await getGstSettings()).toEqual({ built_in: false })
  })
})
