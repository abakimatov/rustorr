import type { Page } from '@playwright/test'

import { expect, MOVIE, removeTorrent, SERIES, test } from './stand'

test('search Rutor and add a result', async ({ page }) => {
  await page.goto('/#/search')
  await removeTorrent(page, SERIES)
  await page.getByPlaceholder('Название фильма').fill('series')
  await page.getByRole('button', { name: 'Найти' }).click()
  const result = page.locator('main li', { hasText: 'Сериал / Series' })
  await result.getByRole('button', { name: 'Добавить' }).click()
  await expect(result.getByRole('link', { name: 'Добавлен' })).toBeVisible({ timeout: 60_000 })
  await result.getByRole('link', { name: 'Добавлен' }).click()
  await expect(page).toHaveURL(new RegExp(`#/torrent/${SERIES}$`))
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Сериал / Series (2020) S01 BDRip 720p')
})

test('settings count changes across groups and discard them', async ({ page }) => {
  await page.goto('/#/settings/cache')
  // The save bar's status (a range's <output> is a status too).
  const status = page.getByRole('status').filter({ hasText: /Измен/ })
  await expect(status).toHaveText('Изменений нет')
  await page.getByLabel('Размер кэша').fill('256')
  await page.getByRole('link', { name: 'Просмотр' }).click()
  await page.getByRole('switch', { name: /Общий плейлист/ }).click()
  await expect(status).toHaveText('Изменено 2 параметра')
  await page.getByRole('button', { name: 'Отменить изменения' }).click()
  await expect(status).toHaveText('Изменений нет')
  await page.getByRole('link', { name: 'GStreamer' }).click()
  await expect(page.getByText('gst-discoverer')).toBeVisible()
})

const routes = [
  '/#/',
  `/#/torrent/${MOVIE}`,
  '/#/search',
  '/#/settings/cache',
  '/#/settings/network',
  '/#/settings/search',
  '/#/settings/waf',
  '/#/settings/gstreamer',
  '/#/settings/storage',
  '/#/settings/about',
]

/** Text that looks like an i18n key left untranslated, e.g. `torrent.files`. */
const rawKey = /\b(app|nav|language|theme|common|status|category|torrents|add|torrent|player|settings|search|cache)\.[a-zA-Z]+[a-zA-Z0-9_.]*\b/

async function checkPage(page: Page, route: string) {
  await page.goto(route)
  await expect(page.locator('main h1')).toBeVisible()
  await page.waitForLoadState('networkidle')
  const text = await page.locator('body').innerText()
  expect(text.match(rawKey)?.[0], `untranslated key on ${route}`).toBeUndefined()
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  expect(overflow, `horizontal overflow on ${route}`).toBeLessThanOrEqual(0)
}

for (const language of ['ru', 'en'] as const) {
  test(`every page is translated and fits the screen (${language})`, async ({ page }) => {
    await page.addInitScript((value) => localStorage.setItem('rustorr.language', value), language)
    for (const route of routes) await checkPage(page, route)
    await expect(page.locator('html')).toHaveAttribute('lang', language)
  })
}

test('every page fits phone, tablet and desktop widths', async ({ page }, info) => {
  test.skip(info.project.name !== 'desktop', 'widths are set explicitly')
  for (const width of [390, 768, 1024, 1440]) {
    await page.setViewportSize({ width, height: 900 })
    for (const route of routes) await checkPage(page, route)
  }
})
