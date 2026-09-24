import type { Language } from '../i18n'

const UNITS: Record<Language, string[]> = {
  ru: ['Б', 'КиБ', 'МиБ', 'ГиБ', 'ТиБ'],
  en: ['B', 'KiB', 'MiB', 'GiB', 'TiB'],
}

/** Binary units, one decimal from KiB up, in the language's number style. */
export function formatBytes(bytes: number | undefined, language: Language): string {
  if (bytes === undefined || !Number.isFinite(bytes) || bytes < 0) return '—'
  const units = UNITS[language]
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const number = new Intl.NumberFormat(language, {
    maximumFractionDigits: unit === 0 ? 0 : 1,
  }).format(value)
  return `${number}\u00a0${units[unit]}`
}

export function formatSpeed(bytesPerSecond: number | undefined, language: Language): string {
  if (!bytesPerSecond || bytesPerSecond < 1) return '—'
  return `${formatBytes(bytesPerSecond, language)}/${language === 'ru' ? 'с' : 's'}`
}

export function formatPeers(active?: number, total?: number): string {
  if (active === undefined && total === undefined) return '—'
  return `${active ?? 0} / ${total ?? 0}`
}

export function formatPercent(part: number, whole: number): string {
  if (whole <= 0) return '0 %'
  return `${Math.min(100, Math.round((part / whole) * 100))} %`
}
