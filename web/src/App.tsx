import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

import { serverVersion } from './api/server'

export function App() {
  const { t } = useTranslation()
  const version = useQuery({ queryKey: ['server', 'version'], queryFn: serverVersion })

  return (
    <main className="mx-auto flex min-h-dvh max-w-3xl flex-col gap-4 p-6">
      <h1 className="font-display text-3xl font-bold tracking-tight">{t('app.title')}</h1>
      <p className="text-muted">
        {version.isSuccess
          ? t('app.server', { version: version.data })
          : version.isError
            ? t('app.unreachable')
            : t('app.connecting')}
      </p>
    </main>
  )
}
