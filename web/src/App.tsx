import { useTranslation } from 'react-i18next'

import { Shell } from './app/Shell'
import { SettingsPage } from './features/settings/SettingsPage'
import { TorrentPage } from './features/torrents/TorrentPage'
import { TorrentsPage } from './features/torrents/TorrentsPage'
import { useRoute } from './lib/router'

function Placeholder({ title }: { title: string }) {
  const { t } = useTranslation()
  return (
    <main className="flex flex-col gap-3 px-4 py-5 lg:px-10 lg:py-8">
      <h1 className="font-display text-3xl font-bold tracking-tight">{title}</h1>
      <p className="text-muted">{t('app.soon')}</p>
    </main>
  )
}

export function App() {
  const { t } = useTranslation()
  const route = useRoute()
  return (
    <Shell route={route}>
      {route.name === 'torrents' && <TorrentsPage />}
      {route.name === 'torrent' && <TorrentPage key={route.hash} hash={route.hash} file={route.file} />}
      {route.name === 'search' && <Placeholder title={t('nav.search')} />}
      {route.name === 'settings' && <SettingsPage section={route.section} />}
    </Shell>
  )
}
