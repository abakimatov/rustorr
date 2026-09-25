import { api, CLIP, expect, FIXTURES, magnet, MOVIE, removeTorrent, test } from './stand'

/** The R8 exit path: add a torrent, open a file, watch it in the browser
 * over HLS and directly, take the M3U and the link for an external player. */
test('add by magnet, watch over HLS and directly, take player links', async ({ page }) => {
  await page.goto('/')
  await removeTorrent(page, MOVIE)

  await page.getByRole('button', { name: 'Добавить' }).first().click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel(/Magnet-ссылка/).fill(magnet(MOVIE, 'movie.mkv'))
  await dialog.getByLabel('Название').fill('Tears of Steel')
  await dialog.getByRole('button', { name: 'Добавить' }).click()
  await expect(dialog).toBeHidden()
  const row = page.locator('li', { hasText: 'Tears of Steel' })
  await expect(row).toBeVisible()

  const video = page.locator('video')
  const playing = () => video.evaluate((element: HTMLVideoElement) => element.currentTime)
  await row.getByRole('link', { name: 'Смотреть' }).click()
  await expect(page).toHaveURL(new RegExp(`#/torrent/${MOVIE}/1$`))

  // The server's HLS is H.264 and AAC in fMP4. A browser that decodes them
  // must play it; one that does not must say so instead of stalling.
  const hlsPlayable = await page.evaluate(() => MediaSource.isTypeSupported('video/mp4; codecs="avc1.4d401f,mp4a.40.2"'))
  test.info().annotations.push({ type: 'hls', description: hlsPlayable ? 'played in the browser' : 'codecs unavailable: error shown, stream checked over HTTP' })
  const master = page.waitForRequest((request) => request.url().includes(`/gst/${MOVIE}/master.m3u8`), { timeout: 90_000 })
  const mode = page.getByRole('group', { name: 'Способ воспроизведения' })
  const hls = mode.getByRole('button', { name: 'HLS' })
  if (!hlsPlayable) await hls.click()
  await expect(hls).toHaveAttribute('aria-pressed', 'true')
  await master
  if (hlsPlayable) {
    await expect.poll(playing, { timeout: 90_000 }).toBeGreaterThan(0.5)
  } else {
    await expect(page.getByRole('alert')).toContainText('H.264 и AAC', { timeout: 90_000 })
  }
  // The repackaged stream itself: variant playlist, init segment, a segment.
  const variant = await (await page.request.get(`/gst/${MOVIE}/video.m3u8?audio=0`)).text()
  expect(variant).toContain('#EXT-X-MAP:URI="init.mp4')
  const init = await page.request.get(`/gst/${MOVIE}/init.mp4?audio=0`)
  expect(init.status()).toBe(200)
  const segment = await page.request.get(`/gst/${MOVIE}/seg/0.m4s?audio=0`)
  expect(segment.status()).toBe(200)
  expect((await segment.body()).length).toBeGreaterThan(1_000)

  // Directly: the browser gets the raw file over /play; whether it decodes
  // Matroska with H.264 is up to it, but it must either play or say so.
  await mode.getByRole('button', { name: 'Напрямую' }).click()
  await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.currentSrc)).toContain(`/play/${MOVIE}/1`)
  await expect
    .poll(async () => (await playing()) > 0.5 || (await page.getByRole('alert').count()) > 0, { timeout: 60_000 })
    .toBe(true)

  // The torrent's M3U lists the file's stream for external players.
  const playlist = await page.getByRole('link', { name: 'Плейлист M3U' }).first().getAttribute('href')
  expect(playlist).toBeTruthy()
  const m3u = await (await page.request.get(playlist ?? '')).text()
  expect(m3u.startsWith('#EXTM3U')).toBe(true)
  expect(m3u).toContain(`link=${MOVIE}`)

  // The copied link streams the file (plain HTTP: the fallback copy path).
  await page.evaluate(() => {
    const original = document.execCommand.bind(document)
    document.execCommand = (command: string, ...rest: [boolean?, string?]) => {
      if (command === 'copy') (window as unknown as { copied: string }).copied = String(window.getSelection() ?? '') || (document.activeElement as HTMLTextAreaElement | null)?.value || ''
      return original(command, ...rest)
    }
  })
  await page.getByRole('button', { name: 'Копировать ссылку для плеера' }).first().click()
  const link = await page.evaluate(() => (window as unknown as { copied?: string }).copied ?? '')
  expect(link).toMatch(new RegExp(`/stream/movie\\.mkv\\?link=${MOVIE}&index=1&play$`))
  const stream = await page.request.get(link, { headers: { Range: 'bytes=0-99' } })
  expect(stream.status()).toBe(206)
  expect((await stream.body()).length).toBe(100)
})

test('upload a .torrent file and play audio', async ({ page }) => {
  await page.goto('/')
  await removeTorrent(page, CLIP)
  await page.getByRole('button', { name: 'Добавить' }).first().click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('tab', { name: 'Файлы .torrent' }).click()
  await dialog.locator('input[type=file]').setInputFiles(`${FIXTURES}/clip.torrent`)
  await expect(dialog.getByText('Выбран 1 файл')).toBeVisible()
  await dialog.getByRole('button', { name: 'Добавить' }).click()
  await expect(dialog).toBeHidden()

  await page.locator('li', { hasText: 'clip.wav' }).getByRole('link', { name: 'Смотреть' }).click()
  const audio = page.locator('audio')
  await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime), { timeout: 60_000 }).toBeGreaterThan(0.3)
  const viewed = await api<{ file_index: number }[]>(page, '/viewed', { action: 'list', hash: CLIP })
  expect(viewed.map((entry) => entry.file_index)).toContain(1)
})
