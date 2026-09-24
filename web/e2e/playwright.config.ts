import { defineConfig, devices } from '@playwright/test'

/** Runs against a Rustorr on the fixture stand (`tools/r4.sh e2e`), or any
 * server given by RUSTORR_E2E_URL. The tests share one server, so they run
 * one at a time. */
const chromium = {
  // The full Chromium build rather than the headless shell.
  channel: 'chromium',
  // Downloads may outlast the click's user activation; a person would press
  // play, the tests let playback start by itself.
  launchOptions: { args: ['--autoplay-policy=no-user-gesture-required'] },
}

export default defineConfig({
  testDir: '.',
  outputDir: 'results',
  globalSetup: './setup.ts',
  reporter: [['list'], ['html', { outputFolder: 'report', open: 'never' }], ['json', { outputFile: 'results/results.json' }]],
  timeout: 150_000,
  expect: { timeout: 30_000 },
  workers: 1,
  retries: 0,
  use: {
    baseURL: process.env.RUSTORR_E2E_URL ?? 'http://localhost:8090',
    locale: 'ru-RU',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'], ...chromium, viewport: { width: 1440, height: 900 } } },
    { name: 'phone', use: { ...devices['Pixel 7'], ...chromium } },
    {
      // Firefox decodes the HLS codecs (see tools/r8/Dockerfile.e2e): the
      // main path plays the stream in the browser.
      name: 'firefox',
      testMatch: /main-path/,
      use: {
        ...devices['Desktop Firefox'],
        viewport: { width: 1440, height: 900 },
        launchOptions: { firefoxUserPrefs: { 'media.autoplay.default': 0, 'media.autoplay.blocking_policy': 0 } },
      },
    },
  ],
})
