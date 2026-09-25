import { expect, test as base, type Page } from '@playwright/test'

/** Fixture torrents the stand's seeder shares (tools/baseline). */
export const MOVIE = '4221aa85bf9bcae8b6f4a4710499fe34a7b345c3'
export const CLIP = '95d1359c6ed7a1efa9e5bf9fab4d2cadbee9fdbf'
export const SERIES = '68c3ccdd52b2925f4f97e2f61ea248e728e54cea'
export const TRACKER = 'http://tracker:6969/announce'
export const FIXTURES = '/fixtures/torrents'

export function magnet(hash: string, name: string): string {
  return `magnet:?xt=urn:btih:${hash}&dn=${encodeURIComponent(name)}&tr=${encodeURIComponent(TRACKER)}`
}

export async function api<T = unknown>(page: Page, path: string, body: unknown): Promise<T> {
  const response = await page.request.post(path, { data: body })
  expect(response.ok(), `${path} → ${response.status()}`).toBe(true)
  const text = await response.text()
  return (text ? JSON.parse(text) : undefined) as T
}

export async function removeTorrent(page: Page, hash: string) {
  await api(page, '/torrents', { action: 'rem', hash })
}

/** Every test fails on a console error or an uncaught exception. */
export const test = base.extend<{ consoleErrors: string[] }>({
  consoleErrors: [
    async ({ page }, use) => {
      const errors: string[] = []
      page.on('console', (message) => {
        if (message.type() === 'error') errors.push(message.text())
      })
      page.on('pageerror', (error) => errors.push(error.message))
      await use(errors)
      expect(errors, 'console errors').toEqual([])
    },
    { auto: true },
  ],
})

export { expect }
