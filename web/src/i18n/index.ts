import i18next from 'i18next'
import { initReactI18next, useTranslation } from 'react-i18next'

import en from './en.json'
import ru from './ru.json'

export const languages = ['ru', 'en'] as const
export type Language = (typeof languages)[number]

const STORAGE_KEY = 'rustorr.language'

function stored(): Language | undefined {
  try {
    const value = localStorage.getItem(STORAGE_KEY)
    return languages.find((language) => language === value)
  } catch {
    return undefined
  }
}

/** The saved choice, else the browser's first supported language, else
 * Russian. */
export function initialLanguage(preferred: readonly string[] = navigator.languages): Language {
  const saved = stored()
  if (saved) return saved
  for (const tag of preferred) {
    const match = languages.find((language) => tag.toLowerCase().startsWith(language))
    if (match) return match
  }
  return 'ru'
}

export function setLanguage(language: Language) {
  try {
    localStorage.setItem(STORAGE_KEY, language)
  } catch {
    // Private windows may refuse storage; the choice lasts for the session.
  }
  document.documentElement.lang = language
  return i18next.changeLanguage(language)
}

export function initI18n(language: Language = initialLanguage()) {
  document.documentElement.lang = language
  return i18next.use(initReactI18next).init({
    resources: { ru: { translation: ru }, en: { translation: en } },
    lng: language,
    fallbackLng: 'ru',
    interpolation: { escapeValue: false },
  })
}

/** The interface language, following changes. */
export function useLanguage(): Language {
  const { i18n } = useTranslation()
  return languages.find((language) => language === i18n.language) ?? 'ru'
}
