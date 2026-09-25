import type { SearchResult } from '../../api/search'

const POWERS: Record<string, number> = { B: 0, K: 1, M: 2, G: 3, T: 4, P: 5 }

/** `4.37 GB`, `640 MiB`, `12,1 ГБ`, MatriX's Torznab `4.4 GCiB` or a plain
 * byte count, in bytes (binary units, as MatriX counts them); 0 when the
 * text is not a size. */
export function parseSize(text: string): number {
  const match = /^\s*([\d.,]+)\s*([a-zа-я]*)\s*$/i.exec(text)
  if (!match) return 0
  const [, number = '', unit = ''] = match
  const value = Number(number.replace(',', '.'))
  if (!Number.isFinite(value)) return 0
  const first = unit
    .charAt(0)
    .toUpperCase()
    .replace('Б', 'B')
    .replace('К', 'K')
    .replace('М', 'M')
    .replace('Г', 'G')
    .replace('Т', 'T')
  const power = unit === '' ? 0 : POWERS[first]
  return power === undefined ? 0 : Math.round(value * 1024 ** power)
}

/** Our category for a Rutor or Torznab one. */
export function categoryFor(categories: string): '' | 'movie' | 'tv' | 'music' | 'other' {
  const text = categories.toLowerCase()
  if (/series|serial|tv|show|сериал/.test(text)) return 'tv'
  if (/movie|film|cartoon|anime|фильм|мульт/.test(text)) return 'movie'
  if (/music|audio|музык/.test(text)) return 'music'
  return text ? 'other' : ''
}

export type SortKey = 'seeds' | 'size' | 'date'

export function sortResults(results: SearchResult[], key: SortKey): SearchResult[] {
  const value = (result: SearchResult) =>
    key === 'seeds' ? result.Seed : key === 'size' ? parseSize(result.Size) : Date.parse(result.CreateDate) || 0
  return [...results].sort((a, b) => value(b) - value(a))
}

/** What to add: the magnet when there is one, else the link to the file. */
export function addLink(result: SearchResult): string {
  return result.Magnet || result.Link
}
