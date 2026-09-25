import { describe, expect, it } from 'vitest'

import { changedKeys, lines } from './draft'

describe('settings draft', () => {
  it('counts changed fields, nested ones by value', () => {
    const saved = { CacheSize: 64, TMDBSettings: { APIKey: '' }, TorznabUrls: null as string[] | null }
    expect(changedKeys(saved, { ...saved })).toEqual([])
    expect(changedKeys(saved, { ...saved, TMDBSettings: { APIKey: '' } })).toEqual([])
    expect(changedKeys(saved, { ...saved, CacheSize: 128, TorznabUrls: [] })).toEqual(['CacheSize', 'TorznabUrls'])
  })

  it('split list text', () => {
    expect(lines(' a \n\n b\n')).toEqual(['a', 'b'])
  })
})
