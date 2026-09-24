import { describe, expect, it } from 'vitest'

import { formatBytes, formatPeers, formatSpeed } from './format'

describe('formatting', () => {
  it('uses binary units in each language', () => {
    expect(formatBytes(4_617_089_843, 'ru')).toBe('4,3 ГиБ')
    expect(formatBytes(4_617_089_843, 'en')).toBe('4.3 GiB')
    expect(formatBytes(512, 'en')).toBe('512 B')
    expect(formatBytes(undefined, 'ru')).toBe('—')
  })

  it('shows speeds and peers, and a dash when idle', () => {
    expect(formatSpeed(1_153_433, 'ru')).toBe('1,1 МиБ/с')
    expect(formatSpeed(0.4, 'en')).toBe('—')
    expect(formatPeers(9, 27)).toBe('9 / 27')
    expect(formatPeers()).toBe('—')
  })
})
