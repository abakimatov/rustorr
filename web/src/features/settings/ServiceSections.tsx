import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import { serverVersion } from '../../api/server'
import {
  type ComponentStatus,
  ffprobeAvailable,
  type GstConfig,
  gstEcho,
  getGstSettings,
  getStorage,
  getWaf,
  resetGstSettings,
  saveGstSettings,
  saveStorage,
  saveWaf,
  shutdownServer,
  type StorageKind,
  type WafLists,
} from '../../api/settings'
import { CheckIcon } from '../../components/icons'
import { Button, StatusDot } from '../../components/ui'
import { changedKeys } from './draft'
import { Group, NumberInput, SelectInput, TextArea, TextInput, Toggle } from './fields'

export const serviceKeys = {
  waf: ['settings', 'waf'] as const,
  gst: ['gst', 'settings'] as const,
  gstEcho: ['gst', 'echo'] as const,
  storage: ['settings', 'storage'] as const,
}

function Saved({ visible }: { visible: boolean }) {
  const { t } = useTranslation()
  if (!visible) return null
  return (
    <span role="status" className="flex items-center gap-1 text-sm text-ok">
      <CheckIcon size={16} />
      {t('settings.saved')}
    </span>
  )
}

function Failure({ error }: { error: unknown }) {
  const { t } = useTranslation()
  if (!error) return null
  return (
    <p role="alert" className="text-sm text-danger">
      {t('common.error', { message: errorText(error) })}
    </p>
  )
}

export function WafSection() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const waf = useQuery({ queryKey: serviceKeys.waf, queryFn: getWaf })
  const [draft, setDraft] = useState<WafLists | null>(null)
  const save = useMutation({
    mutationFn: saveWaf,
    onSuccess: (state) => {
      client.setQueryData(serviceKeys.waf, state)
      setDraft(null)
    },
  })
  if (!waf.data) return <Failure error={waf.error} />
  const lists = draft ?? waf.data
  const dirty = draft !== null && changedKeys<WafLists>(waf.data, draft).length > 0
  const edit = (patch: Partial<WafLists>) => setDraft({ ...lists, ...patch })
  const readOnly = waf.data.read_only

  return (
    <Group title={t('settings.waf.title')} description={t('settings.waf.description')}>
      <div className="flex flex-wrap gap-x-5 gap-y-1 text-sm">
        <span className="flex items-center gap-2">
          <StatusDot tone={waf.data.ip_enabled ? 'ok' : 'idle'} />
          {t(waf.data.ip_enabled ? 'settings.waf.ipOn' : 'settings.waf.ipOff')}
        </span>
        <span className="flex items-center gap-2">
          <StatusDot tone={waf.data.referer_enabled ? 'ok' : 'idle'} />
          {t(waf.data.referer_enabled ? 'settings.waf.refererOn' : 'settings.waf.refererOff')}
        </span>
      </div>
      <TextArea
        label={t('settings.waf.whitelist')}
        hint={t('settings.waf.whitelistHint')}
        placeholder={'192.168.1.0/24\n10.0.0.5'}
        value={lists.whitelist}
        disabled={readOnly}
        onChange={(whitelist) => edit({ whitelist })}
      />
      <TextArea
        label={t('settings.waf.blacklist')}
        hint={t('settings.waf.blacklistHint')}
        value={lists.blacklist}
        disabled={readOnly}
        onChange={(blacklist) => edit({ blacklist })}
      />
      <TextArea
        label={t('settings.waf.referers')}
        hint={t('settings.waf.referersHint')}
        placeholder="example.com"
        value={lists.referers}
        disabled={readOnly}
        onChange={(referers) => edit({ referers })}
      />
      {waf.data.warnings.length > 0 && (
        <ul className="flex flex-col gap-1 rounded-xl bg-warn/10 p-3 text-sm text-warn">
          {waf.data.warnings.map((warning, index) => (
            <li key={index}>
              {t(`settings.waf.warning.${warning.code}`, {
                list: t(`settings.waf.${warning.list}`),
                line: warning.line ?? '—',
                defaultValue: warning.code,
              })}
            </li>
          ))}
        </ul>
      )}
      {readOnly && <p className="text-sm text-muted">{t('settings.readOnly')}</p>}
      <Failure error={save.error} />
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="primary" disabled={!dirty || save.isPending || readOnly} onClick={() => draft && save.mutate(draft)}>
          {t('common.save')}
        </Button>
        {dirty && <Button onClick={() => setDraft(null)}>{t('settings.discard')}</Button>}
        <Saved visible={save.isSuccess && !dirty} />
      </div>
    </Group>
  )
}

function Component({ label, status }: { label: string; status: ComponentStatus }) {
  const { t } = useTranslation()
  const tone = status.works ? 'ok' : status.found ? 'warn' : 'idle'
  const state = status.works ? t('settings.gst.works') : status.found ? t('settings.gst.broken') : t('settings.gst.missing')
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-line py-2.5 first:border-t-0">
      <StatusDot tone={tone} />
      <span className="grow text-[15px]">{label}</span>
      <span className="font-mono text-sm text-muted">{status.version || state}</span>
      {status.error && <span className="w-full text-[13px] text-danger">{status.error}</span>}
    </li>
  )
}

export function GstSection() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const settings = useQuery({ queryKey: serviceKeys.gst, queryFn: getGstSettings })
  const builtIn = settings.data?.built_in === true
  const echo = useQuery({ queryKey: serviceKeys.gstEcho, queryFn: gstEcho, enabled: builtIn })
  const [draft, setDraft] = useState<GstConfig | null>(null)
  const done = () => {
    setDraft(null)
    void client.invalidateQueries({ queryKey: serviceKeys.gst })
  }
  const save = useMutation({ mutationFn: saveGstSettings, onSuccess: done })
  const reset = useMutation({ mutationFn: resetGstSettings, onSuccess: done })

  if (!settings.data) return <Failure error={settings.error} />
  if (!settings.data.built_in) {
    return (
      <Group title={t('settings.gst.title')}>
        <p className="text-muted">{t('settings.gst.notBuiltIn')}</p>
      </Group>
    )
  }
  const saved = settings.data.config
  const config = draft ?? saved
  const dirty = draft !== null && changedKeys(saved, draft).length > 0
  const edit = (patch: Partial<GstConfig>) => setDraft({ ...config, ...patch })
  const flag = (key: keyof GstConfig, label: string, hint?: string) => (
    <Toggle label={label} hint={hint} checked={config[key] as boolean} onChange={(value) => edit({ [key]: value })} />
  )

  return (
    <Group title={t('settings.gst.title')} description={t('settings.gst.description')}>
      {echo.data && (
        <ul className="rounded-xl border border-line px-4">
          <Component label="GStreamer" status={echo.data.gstreamer} />
          <Component label="gst-discoverer" status={echo.data.gst_discoverer} />
          <Component label={t('settings.gst.toneMapping')} status={echo.data.hdr_tone_mapping} />
        </ul>
      )}
      <SelectInput
        label={t('settings.gst.source')}
        hint={t('settings.gst.sourceHint')}
        value={config.Source === 'play' ? 'play' : 'stream'}
        options={[
          { value: 'stream', label: '/stream' },
          { value: 'play', label: '/play' },
        ]}
        onChange={(Source) => edit({ Source })}
      />
      <div className="grid gap-5 sm:grid-cols-2">
        <NumberInput
          label={t('settings.gst.segment')}
          value={config.SegmentSeconds}
          min={1}
          suffix={t('settings.seconds')}
          onChange={(SegmentSeconds) => edit({ SegmentSeconds })}
        />
        <NumberInput
          label={t('settings.gst.segmentDiff')}
          hint={t('settings.gst.segmentDiffHint')}
          value={config.SegmentDiff}
          min={0}
          onChange={(SegmentDiff) => edit({ SegmentDiff })}
        />
        <NumberInput
          label={t('settings.gst.inactive')}
          value={config.InactiveMinutes}
          min={1}
          suffix={t('settings.minutes')}
          onChange={(InactiveMinutes) => edit({ InactiveMinutes })}
        />
        <NumberInput
          label={t('settings.gst.maxTasks')}
          hint={t('settings.gst.maxTasksHint')}
          value={config.MaxTasks}
          min={0}
          onChange={(MaxTasks) => edit({ MaxTasks })}
        />
        <NumberInput
          label={t('settings.gst.aacBitrate')}
          value={config.AACBitrateKbps}
          min={32}
          suffix={t('settings.network.kbitps')}
          onChange={(AACBitrateKbps) => edit({ AACBitrateKbps })}
        />
        <NumberInput
          label={t('settings.gst.aacChannels')}
          hint={t('settings.gst.autoHint')}
          value={config.AACChannels}
          min={0}
          max={8}
          onChange={(AACChannels) => edit({ AACChannels })}
        />
        <NumberInput
          label={t('settings.gst.aacRate')}
          hint={t('settings.gst.autoHint')}
          value={config.AACSamplerate}
          min={0}
          suffix={t('settings.gst.hz')}
          onChange={(AACSamplerate) => edit({ AACSamplerate })}
        />
        <NumberInput
          label={t('settings.gst.videoBitrate')}
          value={config.VideoBitrate}
          min={100}
          suffix={t('settings.network.kbitps')}
          onChange={(VideoBitrate) => edit({ VideoBitrate })}
        />
      </div>
      {flag('Subtitles', t('settings.gst.subtitles'), t('settings.gst.subtitlesHint'))}
      <h3 className="text-sm font-semibold">{t('settings.gst.transcode')}</h3>
      <div className="grid gap-4 sm:grid-cols-2">
        {flag('TranscodeH264', 'H.264')}
        {flag('TranscodeH265', 'H.265 / HEVC')}
        {flag('TranscodeAV1', 'AV1')}
        {flag('TranscodeVP9', 'VP9')}
        {flag('TranscodeVP8', 'VP8')}
        {flag('TranscodeAVI', t('settings.gst.avi'))}
      </div>
      {flag('HDRToSDR', t('settings.gst.hdr'), t('settings.gst.hdrHint'))}
      {flag('HardwareAcceleration', t('settings.gst.hardware'))}
      {flag('UseGPU', t('settings.gst.gpu'))}
      {flag('X264Ultrafast', t('settings.gst.ultrafast'), t('settings.gst.ultrafastHint'))}
      <TextInput
        label={t('settings.gst.path')}
        hint={t('settings.gst.pathHint')}
        value={config.GSTPath}
        onChange={(GSTPath) => edit({ GSTPath })}
      />
      <Failure error={save.error ?? reset.error} />
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="primary" disabled={!dirty || save.isPending} onClick={() => draft && save.mutate(draft)}>
          {t('common.save')}
        </Button>
        {dirty && <Button onClick={() => setDraft(null)}>{t('settings.discard')}</Button>}
        <Button
          variant="ghost"
          disabled={reset.isPending}
          onClick={() => window.confirm(t('settings.gst.resetConfirm')) && reset.mutate()}
        >
          {t('settings.defaults')}
        </Button>
        <Saved visible={(save.isSuccess || reset.isSuccess) && !dirty} />
      </div>
    </Group>
  )
}

export function StorageSection() {
  const { t } = useTranslation()
  const client = useQueryClient()
  const storage = useQuery({ queryKey: serviceKeys.storage, queryFn: getStorage })
  const [draft, setDraft] = useState<{ settings: StorageKind; viewed: StorageKind } | null>(null)
  const save = useMutation({
    mutationFn: saveStorage,
    onSuccess: () => {
      setDraft(null)
      void client.invalidateQueries({ queryKey: ['settings'] })
    },
  })
  if (!storage.data) return <Failure error={storage.error} />
  const choice = draft ?? { settings: storage.data.settings, viewed: storage.data.viewed }
  const dirty = choice.settings !== storage.data.settings || choice.viewed !== storage.data.viewed
  const options = [
    { value: 'json' as const, label: 'JSON' },
    { value: 'bbolt' as const, label: t('settings.storage.database') },
  ]
  return (
    <Group title={t('settings.storage.title')} description={t('settings.storage.description')}>
      <SelectInput
        label={t('settings.storage.settings')}
        value={choice.settings}
        options={options}
        onChange={(settings) => setDraft({ ...choice, settings })}
      />
      <SelectInput
        label={t('settings.storage.viewed')}
        hint={t('settings.storage.viewedCount', { count: storage.data.viewedCount })}
        value={choice.viewed}
        options={options}
        onChange={(viewed) => setDraft({ ...choice, viewed })}
      />
      <Failure error={save.error} />
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate(choice)}>
          {t('common.save')}
        </Button>
        <Saved visible={save.isSuccess && !dirty} />
      </div>
    </Group>
  )
}

/** What this build and start-up enabled. */
export function Capabilities() {
  const { t } = useTranslation()
  const gst = useQuery({ queryKey: serviceKeys.gst, queryFn: getGstSettings })
  const builtIn = gst.data?.built_in === true
  const echo = useQuery({ queryKey: serviceKeys.gstEcho, queryFn: gstEcho, enabled: builtIn })
  const ffprobe = useQuery({ queryKey: ['capability', 'ffprobe'], queryFn: ffprobeAvailable })
  const rows: { label: string; on: boolean | undefined; state: string }[] = [
    {
      label: t('settings.about.hls'),
      on: gst.data ? builtIn && echo.data?.gstreamer.works !== false : undefined,
      state: builtIn ? (echo.data?.gstreamer.version ?? t('settings.about.builtIn')) : t('settings.about.notBuilt'),
    },
    { label: 'ffprobe', on: ffprobe.data, state: ffprobe.data ? t('settings.about.found') : t('settings.about.notFound') },
  ]
  return (
    <section className="flex flex-col gap-3 rounded-2xl border border-line bg-surface p-5">
      <h2 className="font-display text-lg font-bold">{t('settings.about.capabilities')}</h2>
      <ul>
        {rows.map((row) => (
          <li key={row.label} className="flex items-center gap-3 border-t border-line py-2.5 first:border-t-0">
            <StatusDot tone={row.on ? 'ok' : 'idle'} />
            <span className="grow text-[15px]">{row.label}</span>
            <span className="text-sm text-muted">{row.on === undefined ? '…' : row.state}</span>
          </li>
        ))}
      </ul>
    </section>
  )
}

export function Shutdown() {
  const { t } = useTranslation()
  const stop = useMutation({ mutationFn: shutdownServer })
  if (stop.isSuccess) {
    return (
      <p role="status" className="rounded-xl border border-line p-4 text-sm">
        {t('settings.about.stopped')}
      </p>
    )
  }
  return (
    <div className="flex flex-col gap-2">
      <Button
        variant="danger"
        disabled={stop.isPending}
        onClick={() => window.confirm(t('settings.about.stopConfirm')) && stop.mutate()}
      >
        {t('settings.about.stop')}
      </Button>
      <Failure error={stop.error} />
    </div>
  )
}

export function AboutSection() {
  const { t } = useTranslation()
  const version = useQuery({ queryKey: ['server', 'version'], queryFn: serverVersion })
  return (
    <Group title={t('settings.about.title')}>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-2 text-[15px]">
        <dt className="text-muted">{t('settings.about.server')}</dt>
        <dd className="font-mono">{version.data ?? '…'}</dd>
        <dt className="text-muted">{t('settings.about.compat')}</dt>
        <dd>{t('settings.about.compatValue')}</dd>
      </dl>
      <p className="text-sm text-muted">{t('settings.about.text')}</p>
      <div className="flex flex-col gap-4 xl:hidden">
        <Capabilities />
        <Shutdown />
      </div>
    </Group>
  )
}
