import { useSyncExternalStore } from 'react'

/** Screens are addressed after `#`, so the server keeps serving only `/` and
 * unknown paths keep their 404. */
export type Route =
  | { name: 'torrents' }
  | { name: 'torrent'; hash: string; file?: number }
  | { name: 'search' }
  | { name: 'settings'; section?: string }

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent)
  switch (parts[0]) {
    case 'torrent':
      if (!parts[1]) return { name: 'torrents' }
      return /^\d+$/.test(parts[2] ?? '')
        ? { name: 'torrent', hash: parts[1], file: Number(parts[2]) }
        : { name: 'torrent', hash: parts[1] }
    case 'search':
      return { name: 'search' }
    case 'settings':
      return { name: 'settings', section: parts[1] }
    default:
      return { name: 'torrents' }
  }
}

export function href(route: Route): string {
  switch (route.name) {
    case 'torrents':
      return '#/'
    case 'torrent':
      return `#/torrent/${encodeURIComponent(route.hash)}${route.file === undefined ? '' : `/${route.file}`}`
    case 'search':
      return '#/search'
    case 'settings':
      return route.section ? `#/settings/${encodeURIComponent(route.section)}` : '#/settings'
  }
}

function subscribe(onChange: () => void) {
  window.addEventListener('hashchange', onChange)
  return () => window.removeEventListener('hashchange', onChange)
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash)
  return parseRoute(hash)
}

export function navigate(route: Route) {
  window.location.hash = href(route)
}
