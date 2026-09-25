import { afterEach, describe, expect, it, vi } from 'vitest'

import { HttpError, postJson } from './http'
import { serverVersion } from './server'

function respond(status: number, body: string) {
  const fetch = vi.fn().mockResolvedValue(new Response(body, { status }))
  vi.stubGlobal('fetch', fetch)
  return fetch
}

afterEach(() => vi.unstubAllGlobals())

describe('the HTTP client', () => {
  it('reads the server version from /echo', async () => {
    const fetch = respond(200, 'MatriX.145')
    await expect(serverVersion()).resolves.toBe('MatriX.145')
    expect(fetch).toHaveBeenCalledWith('/echo', { credentials: 'same-origin' })
  })

  it('posts JSON and decodes the answer', async () => {
    const fetch = respond(200, '{"status":"ok"}')
    await expect(postJson('/settings', { action: 'get' })).resolves.toEqual({ status: 'ok' })
    const [path, init] = fetch.mock.calls[0] as [string, RequestInit]
    expect(path).toBe('/settings')
    expect(init.method).toBe('POST')
    expect(init.body).toBe('{"action":"get"}')
  })

  it('turns an error status into an HttpError with the body', async () => {
    respond(400, '{"error":"hash is empty"}')
    const error = await postJson('/torrents', {}).catch((caught: unknown) => caught)
    expect(error).toBeInstanceOf(HttpError)
    expect((error as HttpError).status).toBe(400)
    expect((error as HttpError).body).toBe('{"error":"hash is empty"}')
  })
})
