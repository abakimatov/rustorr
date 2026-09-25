import { useEffect, useState } from 'react'

import { copyText } from './clipboard'

/** Copies and remembers which item was copied for a moment, for a "copied"
 * mark next to it. */
export function useCopy(): [string | null, (key: string, text: string) => void] {
  const [copied, setCopied] = useState<string | null>(null)
  useEffect(() => {
    if (copied === null) return
    const timer = window.setTimeout(() => setCopied(null), 1_500)
    return () => window.clearTimeout(timer)
  }, [copied])
  return [
    copied,
    (key, text) => {
      void copyText(text).then((ok) => {
        if (ok) setCopied(key)
      })
    },
  ]
}
