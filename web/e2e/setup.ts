import { request } from '@playwright/test'

import { TRACKER } from './stand'

/** Server settings the scenarios rely on: Rutor search on the fixture
 * database, the stand's tracker for magnets without one, and saved playback
 * positions. Applying settings reconnects the client, so it happens once,
 * before any torrent is added. */
export default async function setup() {
  const context = await request.newContext({ baseURL: process.env.RUSTORR_E2E_URL ?? 'http://localhost:8090' })
  for (let attempt = 0; ; attempt++) {
    const echo = await context.get('/echo').catch(() => undefined)
    if (echo?.ok()) break
    if (attempt > 60) throw new Error('Rustorr did not answer /echo')
    await new Promise((resolve) => setTimeout(resolve, 1_000))
  }
  const settings = await (await context.post('/settings', { data: { action: 'get' } })).json()
  const sets = {
    ...settings,
    EnableRutorSearch: true,
    DefaultTrackers: TRACKER,
    RetrackersMode: 1,
    TrackTimecode: true,
  }
  const saved = await context.post('/settings', { data: { action: 'set', sets } })
  if (!saved.ok()) throw new Error(`settings: ${saved.status()}`)
  await context.dispose()
}
