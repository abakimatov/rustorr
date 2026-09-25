import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import { allPlaylistUrl, playlistUrl } from '../../api/links'
import { torrentFiles } from '../../api/torrents'
import type { Torrent } from '../../api/types'
import { EjectIcon, MoreIcon, PlayIcon, PlaylistIcon, PlusIcon, SearchIcon, TrashIcon } from '../../components/icons'
import { Menu, MenuItem } from '../../components/Menu'
import { Button, Chip, StatusDot } from '../../components/ui'
import { useLanguage } from '../../i18n'
import { formatBytes, formatPeers, formatSpeed } from '../../lib/format'
import { href } from '../../lib/router'
import { mediaKind } from '../player/media'
import { AddDialog } from './AddDialog'
import { useDrop, useRemove, useTorrents } from './queries'
import { initials, isLive, statusKey, statusTone } from './status'

type Filter = { kind: 'all' } | { kind: 'live' } | { kind: 'saved' } | { kind: 'category'; value: string }

function matches(torrent: Torrent, filter: Filter, text: string): boolean {
  const title = `${torrent.title} ${torrent.name ?? ''}`.toLowerCase()
  if (text && !title.includes(text.toLowerCase())) return false
  switch (filter.kind) {
    case 'all':
      return true
    case 'live':
      return isLive(torrent)
    case 'saved':
      return !isLive(torrent)
    case 'category':
      return torrent.category === filter.value
  }
}

/** A tinted block with initials, until TMDB posters (the torrent's own
 * poster link is shown when it has one). */
function Poster({ torrent, className }: { torrent: Torrent; className: string }) {
  const hue = parseInt(torrent.hash.slice(0, 6), 16) % 360
  if (torrent.poster) {
    return <img src={torrent.poster} alt="" loading="lazy" className={`${className} rounded-lg object-cover`} />
  }
  return (
    <div
      aria-hidden="true"
      style={{ background: `oklch(0.42 0.06 ${hue})` }}
      className={`${className} flex items-end rounded-lg p-1.5 font-display text-[13px] font-bold text-white`}
    >
      {initials(torrent.title || torrent.name || '')}
    </div>
  )
}

function Actions({ torrent }: { torrent: Torrent }) {
  const { t } = useTranslation()
  const remove = useRemove()
  const drop = useDrop()
  const origin = window.location.origin
  // Straight to the player when there is something to play.
  const first = torrentFiles(torrent).find((file) => mediaKind(file.path) !== null)
  return (
    <div className="flex justify-end gap-1.5">
      <a
        href={href({ name: 'torrent', hash: torrent.hash, file: first?.id })}
        aria-label={t('torrents.watch')}
        title={t('torrents.watch')}
        className="flex size-10 items-center justify-center rounded-lg bg-accent-soft text-accent"
      >
        <PlayIcon />
      </a>
      <a
        href={playlistUrl(origin, torrent)}
        aria-label={t('torrents.playlist')}
        title={t('torrents.playlist')}
        className="hidden size-10 items-center justify-center rounded-lg border border-line text-muted hover:text-ink sm:flex"
      >
        <PlaylistIcon size={18} />
      </a>
      <Menu label={t('common.more')} icon={<MoreIcon />}>
        {isLive(torrent) && (
          <MenuItem onSelect={() => drop.mutate(torrent.hash)}>
            <EjectIcon size={18} />
            {t('torrents.drop')}
          </MenuItem>
        )}
        <MenuItem
          danger
          onSelect={() => {
            if (window.confirm(t('torrents.removeConfirm', { title: torrent.title || torrent.hash }))) {
              remove.mutate(torrent.hash)
            }
          }}
        >
          <TrashIcon size={18} />
          {t('torrents.remove')}
        </MenuItem>
      </Menu>
    </div>
  )
}

function Row({ torrent }: { torrent: Torrent }) {
  const { t } = useTranslation()
  const language = useLanguage()
  const files = torrentFiles(torrent).length
  const tone = statusTone(torrent)
  const toneText = tone === 'ok' ? 'text-ok' : tone === 'warn' ? 'text-warn' : 'text-muted'
  return (
    <li className="grid grid-cols-[48px_minmax(0,1fr)_auto] items-center gap-3 border-t border-line px-4 py-3.5 first:border-t-0 md:grid-cols-[56px_minmax(0,1fr)_150px_110px_110px_90px_136px] md:gap-4 md:px-5">
      <Poster torrent={torrent} className="h-16 w-12 md:h-19 md:w-14" />
      <div className="flex min-w-0 flex-col gap-1.5">
        <a
          href={href({ name: 'torrent', hash: torrent.hash })}
          className="truncate text-[15px] font-semibold text-ink no-underline hover:text-accent md:text-base"
        >
          {torrent.title || torrent.name || torrent.hash}
        </a>
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 text-[13px] text-muted">
          <span className={`flex items-center gap-1.5 md:hidden ${toneText}`}>
            <StatusDot tone={tone} />
            {t(statusKey(torrent))}
          </span>
          {torrent.category && <span>{t(`category.${torrent.category}`, { defaultValue: torrent.category })}</span>}
          <span className="hidden font-mono md:inline">{torrent.hash.slice(0, 8)}…</span>
          {files > 0 && <span>{t('torrents.files', { count: files })}</span>}
          <span className="font-mono md:hidden">{formatBytes(torrent.torrent_size, language)}</span>
        </div>
      </div>
      <span className={`hidden items-center gap-2 text-sm md:flex ${toneText}`}>
        <StatusDot tone={tone} />
        {t(statusKey(torrent))}
      </span>
      <span className="hidden text-right font-mono text-sm md:block">{formatBytes(torrent.torrent_size, language)}</span>
      <span className="hidden text-right font-mono text-sm text-muted md:block">{formatSpeed(torrent.download_speed, language)}</span>
      <span className="hidden text-right font-mono text-sm text-muted md:block">
        {/* MatriX omits zero counts, so a loaded torrent without peers reads 0 / 0. */}
        {isLive(torrent) ? formatPeers(torrent.active_peers ?? 0, torrent.total_peers ?? 0) : '—'}
      </span>
      <Actions torrent={torrent} />
    </li>
  )
}

export function TorrentsPage() {
  const { t } = useTranslation()
  const torrents = useTorrents()
  const [filter, setFilter] = useState<Filter>({ kind: 'all' })
  const [text, setText] = useState('')
  const [adding, setAdding] = useState(false)
  const list = useMemo(
    () => (torrents.data ?? []).filter((torrent) => matches(torrent, filter, text.trim())),
    [torrents.data, filter, text],
  )
  const usedCategories = useMemo(
    () => [...new Set((torrents.data ?? []).map((torrent) => torrent.category).filter(Boolean))].sort(),
    [torrents.data],
  )
  const is = (candidate: Filter) =>
    candidate.kind === filter.kind && (candidate.kind !== 'category' || filter.kind !== 'category' || candidate.value === filter.value)

  return (
    <main className="flex flex-col gap-5 px-4 py-5 lg:gap-6 lg:px-10 lg:py-8">
      <header className="flex flex-wrap items-center gap-3 lg:gap-4">
        <h1 className="grow font-display text-3xl font-bold tracking-tight lg:text-[34px]">{t('torrents.title')}</h1>
        <label className="order-last flex h-11 w-full items-center gap-2.5 rounded-[10px] border border-line bg-surface px-3.5 text-muted sm:order-none sm:w-72">
          <SearchIcon size={18} />
          <span className="sr-only">{t('torrents.filter')}</span>
          <input
            type="search"
            value={text}
            onChange={(event) => setText(event.target.value)}
            placeholder={t('torrents.filter')}
            className="h-full grow bg-transparent text-[15px] text-ink outline-none placeholder:text-muted"
          />
        </label>
        <a
          href={allPlaylistUrl(window.location.origin)}
          aria-label={t('torrents.allPlaylist')}
          title={t('torrents.allPlaylist')}
          className="hidden size-11 items-center justify-center rounded-[10px] border border-line text-muted hover:text-ink sm:flex"
        >
          <PlaylistIcon />
        </a>
        <Button variant="primary" onClick={() => setAdding(true)}>
          <PlusIcon size={18} />
          {t('torrents.add')}
        </Button>
      </header>
      <div className="-mx-4 flex gap-2 overflow-x-auto px-4 lg:mx-0 lg:px-0">
        <Chip active={is({ kind: 'all' })} onClick={() => setFilter({ kind: 'all' })}>{t('category.all')}</Chip>
        <Chip active={is({ kind: 'live' })} onClick={() => setFilter({ kind: 'live' })}>{t('torrents.working')}</Chip>
        <Chip active={is({ kind: 'saved' })} onClick={() => setFilter({ kind: 'saved' })}>{t('torrents.inDb')}</Chip>
        {usedCategories.map((category) => (
          <Chip
            key={category}
            active={is({ kind: 'category', value: category })}
            onClick={() => setFilter({ kind: 'category', value: category })}
          >
            {t(`category.${category}`, { defaultValue: category })}
          </Chip>
        ))}
      </div>
      {torrents.isError ? (
        <div role="alert" className="flex items-center gap-4 rounded-2xl border border-line p-6">
          <span className="grow text-danger">{t('common.error', { message: errorText(torrents.error) })}</span>
          <Button onClick={() => void torrents.refetch()}>{t('common.retry')}</Button>
        </div>
      ) : torrents.isPending ? (
        <p className="text-muted">{t('common.loading')}</p>
      ) : list.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-2xl border border-dashed border-line px-6 py-16 text-center">
          <p className="font-display text-xl font-bold">
            {torrents.data.length === 0 ? t('torrents.empty') : t('torrents.nothingFound')}
          </p>
          {torrents.data.length === 0 && <p className="text-muted">{t('torrents.emptyHint')}</p>}
          {torrents.data.length === 0 && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              <PlusIcon size={18} />
              {t('torrents.add')}
            </Button>
          )}
        </div>
      ) : (
        <div className="overflow-hidden rounded-2xl border border-line bg-surface">
          <div className="hidden grid-cols-[56px_minmax(0,1fr)_150px_110px_110px_90px_136px] gap-4 bg-raised px-5 py-3 text-xs font-semibold tracking-wider text-muted uppercase md:grid">
            <span />
            <span>{t('torrents.columns.name')}</span>
            <span>{t('torrents.columns.state')}</span>
            <span className="text-right">{t('torrents.columns.size')}</span>
            <span className="text-right">{t('torrents.columns.speed')}</span>
            <span className="text-right">{t('torrents.columns.peers')}</span>
            <span />
          </div>
          <ul>
            {list.map((torrent) => (
              <Row key={torrent.hash} torrent={torrent} />
            ))}
          </ul>
        </div>
      )}
      <AddDialog open={adding} onClose={() => setAdding(false)} />
    </main>
  )
}
