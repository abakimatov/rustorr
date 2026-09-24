import { useQuery, useQueryClient } from '@tanstack/react-query'
import type Hls from 'hls.js'
import type { LoadPolicy } from 'hls.js'
import { useEffect, useEffectEvent, useRef, useState } from 'react'
import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'

import { audioTracks, gstBuiltIn, heartbeat, masterUrl, probe, type ProbeTrack } from '../../api/gst'
import { errorText } from '../../api/http'
import { playUrl, streamUrl } from '../../api/links'
import { setViewed } from '../../api/torrents'
import type { FileStat } from '../../api/types'
import { CheckIcon, LinkIcon } from '../../components/icons'
import { Button } from '../../components/ui'
import { useCopy } from '../../lib/useCopy'
import { torrentKeys } from '../torrents/queries'
import { defaultMode, formatTime, mediaKind, type Mode } from './media'

/** How often the position is saved while playing, and the task kept warm. */
const SAVE_EVERY_MS = 15_000
const HEARTBEAT_EVERY_MS = 60_000
/** A saved position this close to the start, or to the end (ten seconds,
 * or a tenth of a short file), starts from the beginning. */
const RESUME_FROM_S = 3

function resumeLimit(durationS: number): number {
  return durationS - Math.min(10, durationS / 10)
}

/** The server downloads from peers before it answers, so its playlists and
 * segments may take far longer than a CDN's. */
function patient(firstByteMs: number, retries: number): LoadPolicy {
  return {
    default: {
      maxTimeToFirstByteMs: firstByteMs,
      maxLoadTimeMs: firstByteMs + 60_000,
      timeoutRetry: { maxNumRetry: retries, retryDelayMs: 0, maxRetryDelayMs: 0 },
      errorRetry: { maxNumRetry: retries, retryDelayMs: 1_000, maxRetryDelayMs: 8_000 },
    },
  }
}

function canPlay(mime: string): boolean {
  return document.createElement('video').canPlayType(mime) !== ''
}

function trackLabel(track: ProbeTrack, t: TFunction): string {
  const parts = [track.Title, track.Language && track.Language !== 'und' ? track.Language : '']
    .filter(Boolean)
  const codec = [track.Codec, track.Channels > 0 ? t('player.channels', { count: track.Channels }) : '']
    .filter(Boolean)
    .join(' ')
  return [parts.join(' · ') || t('player.track', { index: track.Index }), codec].filter(Boolean).join(' — ')
}

interface Failure {
  source: string
  message: string
}

export function Player({
  hash,
  file,
  resumeAt,
  onFinished,
}: {
  hash: string
  file: FileStat
  /** Saved position, in seconds. */
  resumeAt: number
  onFinished: () => void
}) {
  const { t } = useTranslation()
  const client = useQueryClient()
  const [copied, copy] = useCopy()
  const media = useRef<HTMLVideoElement & HTMLAudioElement>(null)
  const hls = useRef<Hls | null>(null)
  const start = useRef(resumeAt >= RESUME_FROM_S ? resumeAt : 0)
  const kind = mediaKind(file.path) ?? 'video'

  const gst = useQuery({ queryKey: ['gst', 'builtIn'], queryFn: gstBuiltIn, staleTime: Infinity, retry: false })
  const [chosenMode, setChosenMode] = useState<Mode | null>(null)
  const mode: Mode | null =
    chosenMode ?? (gst.isPending ? null : defaultMode(file.path, gst.data === true, canPlay))
  const hlsAvailable = gst.data === true && kind === 'video'

  const probed = useQuery({
    queryKey: ['gst', 'probe', hash, file.id],
    queryFn: () => probe(hash, file.id),
    enabled: mode === 'hls',
    staleTime: Infinity,
    retry: false,
  })
  const audios = audioTracks(probed.data)
  const [audio, setAudio] = useState<number | null>(null)
  const [subtitle, setSubtitle] = useState(-1)
  const [subtitles, setSubtitles] = useState<{ source: string; names: string[] }>({ source: '', names: [] })
  const [failure, setFailure] = useState<Failure | null>(null)
  const [resumed, setResumed] = useState(resumeAt >= RESUME_FROM_S)

  // HLS waits for the probe (the server needs it for the playlist anyway and
  // keeps it for an hour): it gives the audio tracks and the duration.
  const ready = mode === 'direct' || (mode === 'hls' && !probed.isPending)
  const durationS = (probed.data?.DurationNS ?? 0) / 1e9
  const source = `${mode}|${audio ?? ''}`
  // Where a saved HLS position is too close to the end to resume from.
  const resumeLimitS = mode === 'hls' && durationS > 0 ? resumeLimit(durationS) : Infinity
  const showResumed = resumed && resumeAt < resumeLimitS
  const fileId = file.id
  // Read inside effects without restarting them: the parent re-renders on
  // every statistics poll, and a language switch must not reload the stream.
  const message = useEffectEvent((key: string) => t(key))
  const finished = useEffectEvent(() => onFinished())
  const error = failure?.source === source ? failure.message : null
  const subtitleNames = subtitles.source === source ? subtitles.names : []

  // Attaches the stream; a new mode or audio track starts where playback is.
  useEffect(() => {
    const element = media.current
    if (!element || mode === null || !ready) return
    let cancelled = false
    let player: Hls | undefined
    let at = start.current
    if (at >= resumeLimitS) {
      at = 0
      start.current = 0
    }
    const fail = (text: string) => {
      if (!cancelled) setFailure({ source, message: text })
    }
    const onError = () => {
      if (mode === 'direct' || !hls.current) fail(element.error?.message || message('player.cannotPlay'))
    }
    element.addEventListener('error', onError)

    if (mode === 'direct') {
      element.src = playUrl(hash, fileId)
      element.addEventListener(
        'loadedmetadata',
        () => {
          if (at > 0 && at < resumeLimit(element.duration)) element.currentTime = at
          else if (at > 0 && !cancelled) setResumed(false)
        },
        { once: true },
      )
      element.play().catch(() => undefined)
    } else {
      const url = masterUrl(hash, fileId, audio ?? 0)
      const nativeHls = element.canPlayType('application/vnd.apple.mpegurl') !== ''
      void import('hls.js').then(({ default: HlsClass }) => {
        if (cancelled) return
        if (!HlsClass.isSupported()) {
          if (!nativeHls) return fail(message('player.noHls'))
          element.src = url
          element.addEventListener(
            'loadedmetadata',
            () => {
              if (at > 0) element.currentTime = at
              const names = Array.from(element.textTracks, (track) => track.label || track.language)
              setSubtitles({ source, names })
            },
            { once: true },
          )
          element.play().catch(() => undefined)
          return
        }
        player = new HlsClass({
          startPosition: at > 0 ? at : -1,
          manifestLoadPolicy: patient(120_000, 1),
          playlistLoadPolicy: patient(60_000, 2),
          fragLoadPolicy: patient(120_000, 3),
        })
        hls.current = player
        let recovered = false
        player.on(HlsClass.Events.SUBTITLE_TRACKS_UPDATED, (_event, data) => {
          if (!cancelled) setSubtitles({ source, names: data.subtitleTracks.map((track) => track.name) })
        })
        player.on(HlsClass.Events.MANIFEST_PARSED, () => {
          element.play().catch(() => undefined)
        })
        player.on(HlsClass.Events.ERROR, (_event, data) => {
          if (!data.fatal) return
          if (data.type === HlsClass.ErrorTypes.MEDIA_ERROR && !recovered) {
            recovered = true
            player?.recoverMediaError()
            return
          }
          const status = data.response?.code
          const body = typeof data.response?.data === 'string' ? data.response.data.trim() : ''
          fail(body || (status ? `HTTP ${status}` : data.details))
        })
        player.loadSource(url)
        player.attachMedia(element)
      })
    }

    return () => {
      cancelled = true
      element.removeEventListener('error', onError)
      start.current = element.currentTime > 0 ? element.currentTime : start.current
      player?.destroy()
      hls.current = null
      element.removeAttribute('src')
      element.load()
    }
  }, [mode, audio, hash, fileId, source, ready, resumeLimitS])

  // Subtitles: hls.js renders its tracks natively once selected.
  useEffect(() => {
    const player = hls.current
    if (player) {
      player.subtitleTrack = subtitle
      player.subtitleDisplay = subtitle >= 0
      return
    }
    const element = media.current
    if (!element) return
    Array.from(element.textTracks).forEach((track, index) => {
      track.mode = index === subtitle ? 'showing' : 'disabled'
    })
  }, [subtitle, subtitleNames.length])

  // Remembers the position and marks the file viewed (`/play` does that by
  // itself, `/gst` does not).
  useEffect(() => {
    const element = media.current
    if (!element) return
    let last = 0
    const save = (position = element.currentTime) => {
      last = Date.now()
      void setViewed(hash, fileId, Math.floor(position)).then(() =>
        client.invalidateQueries({ queryKey: torrentKeys.viewed(hash) }),
      )
    }
    const onTime = () => {
      if (Date.now() - last >= SAVE_EVERY_MS && !element.paused) save()
    }
    const onPlaying = () => {
      if (last === 0) save()
    }
    const onPause = () => {
      if (!element.ended && element.currentTime > 0) save()
    }
    const onEnded = () => {
      save(0)
      finished()
    }
    element.addEventListener('timeupdate', onTime)
    element.addEventListener('playing', onPlaying)
    element.addEventListener('pause', onPause)
    element.addEventListener('ended', onEnded)
    return () => {
      element.removeEventListener('timeupdate', onTime)
      element.removeEventListener('playing', onPlaying)
      element.removeEventListener('pause', onPause)
      element.removeEventListener('ended', onEnded)
      if (element.currentTime > 0 && !element.ended) save()
    }
  }, [hash, fileId, client])

  // Keeps the server's HLS task alive while paused.
  useEffect(() => {
    if (mode !== 'hls') return
    const timer = window.setInterval(() => void heartbeat(hash).catch(() => undefined), HEARTBEAT_EVERY_MS)
    return () => window.clearInterval(timer)
  }, [mode, hash])

  const otherMode: Mode | null = mode === 'hls' ? 'direct' : hlsAvailable ? 'hls' : null
  const Element = kind === 'audio' ? 'audio' : 'video'

  return (
    <section aria-label={t('player.label')} className="flex flex-col gap-3">
      <div className={`relative overflow-hidden rounded-2xl bg-black ${kind === 'audio' ? 'p-4' : 'aspect-video'}`}>
        <Element
          ref={media}
          controls
          playsInline
          preload="metadata"
          className={kind === 'audio' ? 'w-full' : 'size-full'}
        />
        {(mode === null || !ready) && (
          <p className="absolute inset-0 flex items-center justify-center text-sm text-white/70">{t('common.loading')}</p>
        )}
        {error && (
          <div role="alert" className="absolute inset-0 flex flex-col items-center justify-center gap-4 bg-black/85 p-6 text-center text-white">
            <p className="font-semibold">{t('player.failed')}</p>
            <p className="max-w-lg text-sm break-words text-white/70">{error}</p>
            <div className="flex flex-wrap justify-center gap-2">
              {otherMode && (
                <Button variant="primary" onClick={() => setChosenMode(otherMode)}>
                  {t(otherMode === 'hls' ? 'player.tryHls' : 'player.tryDirect')}
                </Button>
              )}
              <Button onClick={() => copy('stream', streamUrl(window.location.origin, hash, file))}>
                {copied === 'stream' ? <CheckIcon size={18} /> : <LinkIcon size={18} />}
                {copied === 'stream' ? t('common.copied') : t('torrent.copyLink')}
              </Button>
            </div>
          </div>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        {hlsAvailable && mode !== null && (
          <div role="group" aria-label={t('player.mode')} className="flex rounded-[10px] border border-line bg-surface p-0.5">
            {(['direct', 'hls'] as const).map((value) => (
              <button
                key={value}
                type="button"
                aria-pressed={mode === value}
                onClick={() => setChosenMode(value)}
                className={`h-9 rounded-lg px-3 ${mode === value ? 'bg-accent-soft font-semibold text-accent' : 'text-muted hover:text-ink'}`}
              >
                {t(value === 'hls' ? 'player.hls' : 'player.direct')}
              </button>
            ))}
          </div>
        )}
        {mode === 'hls' && audios.length > 1 && (
          <label className="flex items-center gap-2 text-muted">
            {t('player.audio')}
            <select
              value={audio ?? audios[0]?.Index}
              onChange={(event) => {
                if (media.current) start.current = media.current.currentTime
                setAudio(Number(event.target.value))
              }}
              className="h-9 max-w-64 rounded-lg border border-line bg-surface px-2 text-ink"
            >
              {audios.map((track) => (
                <option key={track.Index} value={track.Index}>
                  {trackLabel(track, t)}
                </option>
              ))}
            </select>
          </label>
        )}
        {subtitleNames.length > 0 && (
          <label className="flex items-center gap-2 text-muted">
            {t('player.subtitles')}
            <select
              value={subtitle}
              onChange={(event) => setSubtitle(Number(event.target.value))}
              className="h-9 max-w-64 rounded-lg border border-line bg-surface px-2 text-ink"
            >
              <option value={-1}>{t('player.off')}</option>
              {subtitleNames.map((name, index) => (
                <option key={index} value={index}>
                  {name}
                </option>
              ))}
            </select>
          </label>
        )}
        {mode === 'hls' && probed.isError && <span className="text-danger">{errorText(probed.error)}</span>}
        {showResumed && (
          <span className="flex items-center gap-2 text-muted">
            {t('player.resumed', { time: formatTime(resumeAt) })}
            <button
              type="button"
              className="font-semibold text-accent"
              onClick={() => {
                if (media.current) media.current.currentTime = 0
                setResumed(false)
              }}
            >
              {t('player.fromStart')}
            </button>
          </span>
        )}
      </div>
      {mode === 'hls' && <p className="text-[13px] text-muted">{t('player.hlsNote')}</p>}
    </section>
  )
}
