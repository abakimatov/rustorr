import { describe, expect, it } from 'vitest'

import en from './en.json'
import { initialLanguage } from './index'
import ru from './ru.json'

function keys(value: unknown, prefix = ''): string[] {
  if (typeof value !== 'object' || value === null) return [prefix]
  return Object.entries(value).flatMap(([key, item]) => keys(item, prefix ? `${prefix}.${key}` : key))
}

describe('translations', () => {
  it('have the same keys in every language', () => {
    expect(keys(en).sort()).toEqual(keys(ru).sort())
  })

  it('follow the browser, falling back to Russian', () => {
    expect(initialLanguage(['en-US', 'ru'])).toBe('en')
    expect(initialLanguage(['de-DE', 'ru-RU'])).toBe('ru')
    expect(initialLanguage(['fr'])).toBe('ru')
  })
})
