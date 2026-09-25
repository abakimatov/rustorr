import { useCallback, useEffect, useState } from 'react'

export type ThemeChoice = 'system' | 'light' | 'dark'
const STORAGE_KEY = 'rustorr.theme'

function stored(): ThemeChoice {
  try {
    const value = localStorage.getItem(STORAGE_KEY)
    return value === 'light' || value === 'dark' ? value : 'system'
  } catch {
    return 'system'
  }
}

function apply(choice: ThemeChoice) {
  if (choice === 'system') delete document.documentElement.dataset.theme
  else document.documentElement.dataset.theme = choice
}

/** The explicit choice wins; otherwise the system setting decides (CSS). */
export function useTheme() {
  const [choice, setChoice] = useState<ThemeChoice>(stored)
  useEffect(() => apply(choice), [choice])
  const choose = useCallback((next: ThemeChoice) => {
    try {
      if (next === 'system') localStorage.removeItem(STORAGE_KEY)
      else localStorage.setItem(STORAGE_KEY, next)
    } catch {
      // Storage may be unavailable; the choice then lasts for the session.
    }
    setChoice(next)
  }, [])
  return { choice, choose }
}
