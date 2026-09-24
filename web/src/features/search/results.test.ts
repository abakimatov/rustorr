import { describe, expect, it } from 'vitest'

import type { SearchResult } from '../../api/search'
import { addLink, categoryFor, parseSize, sortResults } from './results'

const result = (patch: Partial<SearchResult>): SearchResult => ({
  Title: '', Name: '', Names: null, Categories: '', Size: '', CreateDate: '', Tracker: '', Link: '',
  Year: 0, Peer: 0, Seed: 0, Magnet: '', Hash: '', IMDBID: '', ...patch,
})

describe('search results', () => {
  it('read sizes as the sources write them', () => {
    expect(parseSize('4.37 GB')).toBe(Math.round(4.37 * 1024 ** 3))
    expect(parseSize('640 MiB')).toBe(640 * 1024 ** 2)
    expect(parseSize('12,1 ГБ')).toBe(Math.round(12.1 * 1024 ** 3))
    expect(parseSize('4.4 GCiB')).toBe(Math.round(4.4 * 1024 ** 3))
    expect(parseSize('4692251852')).toBe(4692251852)
    expect(parseSize('big')).toBe(0)
  })

  it('map source categories to ours', () => {
    expect(categoryFor('Movie')).toBe('movie')
    expect(categoryFor('CartoonMovie')).toBe('movie')
    expect(categoryFor('Series')).toBe('tv')
    expect(categoryFor('Music')).toBe('music')
    expect(categoryFor('Books')).toBe('other')
    expect(categoryFor('')).toBe('')
  })

  it('sort by seeds, size or date, largest first', () => {
    const a = result({ Title: 'a', Seed: 1, Size: '2 GB', CreateDate: '2021-01-01T00:00:00Z' })
    const b = result({ Title: 'b', Seed: 5, Size: '1 GB', CreateDate: '2020-01-01T00:00:00Z' })
    expect(sortResults([a, b], 'seeds').map((r) => r.Title)).toEqual(['b', 'a'])
    expect(sortResults([b, a], 'size').map((r) => r.Title)).toEqual(['a', 'b'])
    expect(sortResults([b, a], 'date').map((r) => r.Title)).toEqual(['a', 'b'])
  })

  it('prefer the magnet', () => {
    expect(addLink(result({ Magnet: 'magnet:?x', Link: 'http://l' }))).toBe('magnet:?x')
    expect(addLink(result({ Link: 'http://l' }))).toBe('http://l')
  })
})
