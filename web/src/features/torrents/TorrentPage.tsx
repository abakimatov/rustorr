import { useTranslation } from 'react-i18next'

import { errorText, HttpError } from '../../api/http'
import { fileName, magnetLink, playlistUrl, streamUrl } from '../../api/links'
import { torrentFiles } from '../../api/torrents'
import type { FileStat, Torrent, Viewed } from '../../api/types'
import { BackIcon, CheckIcon, CopyIcon, EjectIcon, LinkIcon, PlayIcon, PlaylistIcon, TrashIcon } from '../../components/icons'
import { Button, IconButton, StatusDot } from '../../components/ui'
import { useLanguage } from '../../i18n'
import { formatBytes, formatPeers, formatPercent, formatSpeed } from '../../lib/format'
import { href, navigate } from '../../lib/router'
import { useCopy } from '../../lib/useCopy'
import { mediaKind } from '../player/media'
import { Player } from '../player/Player'
import { CacheMap } from './CachePanel'
import { useDrop, useRemove, useTorrent, useViewed } from './queries'
import { isLive, statusKey, statusTone } from './status'

function isPlayable(file: FileStat): boolean {
  return mediaKind(file.path) !== null
}

function Stat({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return (
    <div className="flex flex-col gap-1 rounded-xl border border-line bg-surface px-4 py-3">
      <span className="text-[13px] text-muted">{label}</span>
      <span className="font-mono text-[15px]">{value}</span>
      {detail && <span className="font-mono text-[13px] text-muted">{detail}</span>}
    </div>
  )
}

function Stats({ torrent }: { torrent: Torrent }) {
  const { t } = useTranslation()
  const language = useLanguage()
  return (
    <section aria-label={t('torrent.stats')} className="grid grid-cols-2 gap-3 md:grid-cols-4">
      <Stat label={t('torrent.download')} value={formatSpeed(torrent.download_speed, language)} />
      <Stat label={t('torrent.upload')} value={formatSpeed(torrent.upload_speed, language)} />
      {/* MatriX omits zero counts. */}
      <Stat label={t('torrent.peers')} value={formatPeers(torrent.active_peers ?? 0, torrent.total_peers ?? 0)} />
      <Stat
        label={t('torrent.loaded')}
        value={torrent.torrent_size ? formatPercent(torrent.loaded_size ?? 0, torrent.torrent_size) : '—'}
        detail={torrent.torrent_size ? formatBytes(torrent.loaded_size ?? 0, language) : undefined}
      />
    </section>
  )
}

function Files({
  torrent,
  files,
  viewed,
  current,
}: {
  torrent: Torrent
  files: FileStat[]
  viewed: Viewed[]
  current?: number
}) {
  const { t } = useTranslation()
  const language = useLanguage()
  const [copied, copy] = useCopy()
  const seen = new Set(viewed.map((entry) => entry.file_index))
  const origin = window.location.origin

  if (files.length === 0) {
    return <p className="rounded-xl border border-dashed border-line p-6 text-muted">{t('torrent.noFiles')}</p>
  }
  return (
    <ul className="overflow-hidden rounded-2xl border border-line bg-surface">
      {files.map((file) => (
        <li
          key={file.id}
          aria-current={file.id === current ? 'true' : undefined}
          className={`flex items-center gap-3 border-t border-line px-4 py-3 first:border-t-0 ${file.id === current ? 'bg-accent-soft' : ''}`}
        >
          <div className="flex min-w-0 grow flex-col gap-1">
            <span className="truncate text-[15px] font-medium" title={file.path}>
              {fileName(file.path)}
            </span>
            <span className="flex flex-wrap gap-x-2.5 text-[13px] text-muted">
              <span className="font-mono">{formatBytes(file.length, language)}</span>
              {file.id === current ? (
                <span className="font-semibold text-accent">{t('player.playing')}</span>
              ) : (
                seen.has(file.id) && (
                  <span className="flex items-center gap-1 text-ok">
                    <CheckIcon size={14} />
                    {t('torrent.viewed')}
                  </span>
                )
              )}
            </span>
          </div>
          <IconButton
            label={copied === `file-${file.id}` ? t('common.copied') : t('torrent.copyLink')}
            onClick={() => copy(`file-${file.id}`, streamUrl(origin, torrent.hash, file))}
          >
            {copied === `file-${file.id}` ? <CheckIcon size={18} className="text-ok" /> : <LinkIcon size={18} />}
          </IconButton>
          {isPlayable(file) && (
            <a
              href={href({ name: 'torrent', hash: torrent.hash, file: file.id })}
              aria-label={t('torrent.watchInBrowser')}
              title={t('torrent.watchInBrowser')}
              className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-accent-soft text-accent"
            >
              <PlayIcon />
            </a>
          )}
        </li>
      ))}
    </ul>
  )
}

function Actions({ torrent, className }: { torrent: Torrent; className: string }) {
  const { t } = useTranslation()
  const remove = useRemove()
  const drop = useDrop()
  const [copied, copy] = useCopy()
  const origin = window.location.origin
  const title = torrent.title || torrent.name || torrent.hash
  return (
    <div className={`gap-2 ${className}`}>
      <a
        href={playlistUrl(origin, torrent)}
        className="col-span-2 inline-flex h-11 items-center justify-center gap-2 rounded-[10px] bg-accent px-4 text-[15px] font-semibold text-on-accent no-underline hover:brightness-110"
      >
        <PlaylistIcon size={18} />
        {t('torrents.playlist')}
      </a>
      <a
        href={playlistUrl(origin, torrent, true)}
        className="col-span-2 inline-flex h-11 items-center justify-center gap-2 rounded-[10px] border border-line bg-surface px-4 text-[15px] text-ink no-underline hover:bg-raised"
      >
        {t('torrent.fromLast')}
      </a>
      <Button onClick={() => copy('magnet', magnetLink(torrent))}>
        {copied === 'magnet' ? <CheckIcon size={18} className="text-ok" /> : <CopyIcon size={18} />}
        {copied === 'magnet' ? t('common.copied') : t('torrent.magnet')}
      </Button>
      {isLive(torrent) && (
        <Button onClick={() => drop.mutate(torrent.hash)} disabled={drop.isPending}>
          <EjectIcon size={18} />
          {t('torrents.drop')}
        </Button>
      )}
      <Button
        variant="danger"
        disabled={remove.isPending}
        onClick={() => {
          if (window.confirm(t('torrents.removeConfirm', { title }))) {
            remove.mutate(torrent.hash, { onSuccess: () => navigate({ name: 'torrents' }) })
          }
        }}
      >
        <TrashIcon size={18} />
        {t('torrents.remove')}
      </Button>
    </div>
  )
}

/** Title, facts and actions; while a file plays, phones get the actions
 * under the player instead, so the video comes first. */
function Header({ torrent, playing = false }: { torrent: Torrent; playing?: boolean }) {
  const { t } = useTranslation()
  const language = useLanguage()
  const tone = statusTone(torrent)
  const toneText = tone === 'ok' ? 'text-ok' : tone === 'warn' ? 'text-warn' : 'text-muted'
  const title = torrent.title || torrent.name || torrent.hash

  return (
    <header className="flex flex-col gap-4 md:flex-row md:items-start">
      {torrent.poster && (
        <img src={torrent.poster} alt="" className="hidden h-44 w-30 shrink-0 rounded-xl object-cover md:block" />
      )}
      <div className="flex min-w-0 grow flex-col gap-3">
        <h1 className="font-display text-2xl font-bold tracking-tight break-words md:text-[32px] md:leading-tight">
          {title}
        </h1>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-sm text-muted">
          <span className={`flex items-center gap-2 ${toneText}`}>
            <StatusDot tone={tone} />
            {t(statusKey(torrent))}
          </span>
          {torrent.category && <span>{t(`category.${torrent.category}`, { defaultValue: torrent.category })}</span>}
          {torrent.torrent_size !== undefined && (
            <span className="font-mono">{formatBytes(torrent.torrent_size, language)}</span>
          )}
          <span className="font-mono break-all">{torrent.hash}</span>
        </div>
        <Actions
          torrent={torrent}
          className={playing ? 'hidden md:flex md:flex-wrap' : 'grid grid-cols-2 sm:flex sm:flex-wrap'}
        />
      </div>
    </header>
  )
}

function Details({ torrent, fileId }: { torrent: Torrent; fileId?: number }) {
  const { t } = useTranslation()
  const viewed = useViewed(torrent.hash)
  const files = torrentFiles(torrent)
  const playing = fileId === undefined ? undefined : files.find((file) => file.id === fileId)
  const playable = files.filter(isPlayable)
  const next = playing && playable[playable.findIndex((file) => file.id === playing.id) + 1]
  const saved = playing && viewed.data?.find((entry) => entry.file_index === playing.id)

  if (!playing) {
    return (
      <>
        <Header torrent={torrent} />
        {isLive(torrent) && <Stats torrent={torrent} />}
        <section className="flex flex-col gap-3">
          <h2 className="font-display text-xl font-bold">{t('torrent.files')}</h2>
          <Files torrent={torrent} files={files} viewed={viewed.data ?? []} />
        </section>
        {isLive(torrent) && <CacheMap hash={torrent.hash} />}
      </>
    )
  }
  return (
    <>
      <Header torrent={torrent} playing />
      <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_380px]">
        <div className="flex min-w-0 flex-col gap-4">
          <h2 className="truncate text-lg font-semibold" title={playing.path}>
            {fileName(playing.path)}
          </h2>
          {/* The saved position is read once, so the player waits for it. */}
          {viewed.isPending ? (
            <div className="aspect-video rounded-2xl bg-black" />
          ) : (
            <Player
              key={`${torrent.hash}/${playing.id}`}
              hash={torrent.hash}
              file={playing}
              resumeAt={saved?.timecode ?? 0}
              onFinished={() => {
                if (next) navigate({ name: 'torrent', hash: torrent.hash, file: next.id })
              }}
            />
          )}
          <Actions torrent={torrent} className="grid grid-cols-2 md:hidden" />
          {isLive(torrent) && <Stats torrent={torrent} />}
        </div>
        <aside className="flex flex-col gap-3">
          <h2 className="font-display text-xl font-bold">{t('torrent.files')}</h2>
          <Files torrent={torrent} files={files} viewed={viewed.data ?? []} current={playing.id} />
          {isLive(torrent) && <CacheMap hash={torrent.hash} />}
        </aside>
      </div>
    </>
  )
}

export function TorrentPage({ hash, file }: { hash: string; file?: number }) {
  const { t } = useTranslation()
  const torrent = useTorrent(hash)
  const missing = torrent.error instanceof HttpError && torrent.error.status === 404

  return (
    <main className="flex flex-col gap-6 px-4 py-5 lg:px-10 lg:py-8">
      <a href={href({ name: 'torrents' })} className="flex w-fit items-center gap-1.5 text-[15px] text-muted no-underline hover:text-ink">
        <BackIcon size={18} />
        {t('torrent.back')}
      </a>
      {torrent.data ? (
        <Details torrent={torrent.data} fileId={file} />
      ) : missing ? (
        <p className="font-display text-xl font-bold">{t('torrent.notFound')}</p>
      ) : torrent.isError ? (
        <div role="alert" className="flex items-center gap-4 rounded-2xl border border-line p-6">
          <span className="grow text-danger">{t('common.error', { message: errorText(torrent.error) })}</span>
          <Button onClick={() => void torrent.refetch()}>{t('common.retry')}</Button>
        </div>
      ) : (
        <p className="text-muted">{t('common.loading')}</p>
      )}
    </main>
  )
}
