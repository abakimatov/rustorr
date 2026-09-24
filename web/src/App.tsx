import { Shell } from './app/Shell'
import { SearchPage } from './features/search/SearchPage'
import { SettingsPage } from './features/settings/SettingsPage'
import { TorrentPage } from './features/torrents/TorrentPage'
import { TorrentsPage } from './features/torrents/TorrentsPage'
import { useRoute } from './lib/router'

export function App() {
  const route = useRoute()
  return (
    <Shell route={route}>
      {route.name === 'torrents' && <TorrentsPage />}
      {route.name === 'torrent' && <TorrentPage key={route.hash} hash={route.hash} file={route.file} />}
      {route.name === 'search' && <SearchPage />}
      {route.name === 'settings' && <SettingsPage section={route.section} />}
    </Shell>
  )
}
