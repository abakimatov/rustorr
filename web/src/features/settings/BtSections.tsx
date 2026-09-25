import { useMutation } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import { type BtSettings, type TorznabConfig, testTorznab } from '../../api/settings'
import { CheckIcon, PlusIcon, TrashIcon } from '../../components/icons'
import { Button, IconButton } from '../../components/ui'
import { useLanguage } from '../../i18n'
import { formatBytes } from '../../lib/format'
import { MIB } from './draft'
import { Group, NotApplied, NumberInput, RangeInput, SelectInput, TextArea, TextInput, Toggle } from './fields'

export interface SectionProps {
  draft: BtSettings
  update: (patch: Partial<BtSettings>) => void
}

export function CacheSection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  const language = useLanguage()
  return (
    <Group title={t('settings.cache.title')} description={t('settings.cache.description')}>
      <NumberInput
        label={t('settings.cache.size')}
        hint={t('settings.cache.sizeHint', { size: formatBytes(draft.CacheSize, language) })}
        value={Math.round(draft.CacheSize / MIB)}
        min={1}
        suffix={t('settings.mib')}
        onChange={(value) => update({ CacheSize: Math.max(1, value) * MIB })}
      />
      <RangeInput
        label={t('settings.cache.preload')}
        hint={t('settings.cache.preloadHint')}
        value={draft.PreloadCache}
        min={0}
        max={100}
        format={(value) => `${value} %`}
        onChange={(value) => update({ PreloadCache: value })}
      />
      <NumberInput
        label={t('settings.cache.disconnect')}
        hint={t('settings.cache.disconnectHint')}
        value={draft.TorrentDisconnectTimeout}
        min={1}
        suffix={t('settings.seconds')}
        onChange={(value) => update({ TorrentDisconnectTimeout: value })}
      />
      <Toggle
        label={t('settings.cache.removeOnDrop')}
        hint={t('settings.cache.removeOnDropHint')}
        checked={draft.RemoveCacheOnDrop}
        onChange={(value) => update({ RemoveCacheOnDrop: value })}
      />
      <NotApplied>
        <RangeInput
          label={t('settings.cache.readAhead')}
          hint={t('settings.cache.readAheadHint')}
          value={draft.ReaderReadAHead}
          min={5}
          max={100}
          format={(value) => `${value} %`}
          onChange={(value) => update({ ReaderReadAHead: value })}
        />
        <Toggle
          label={t('settings.cache.useDisk')}
          checked={draft.UseDisk}
          onChange={(value) => update({ UseDisk: value })}
        />
        <TextInput
          label={t('settings.cache.savePath')}
          value={draft.TorrentsSavePath}
          onChange={(value) => update({ TorrentsSavePath: value })}
        />
      </NotApplied>
    </Group>
  )
}

export function PlaybackSection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  return (
    <Group title={t('settings.playback.title')}>
      <Toggle
        label={t('settings.playback.timecode')}
        hint={t('settings.playback.timecodeHint')}
        checked={draft.TrackTimecode}
        onChange={(value) => update({ TrackTimecode: value })}
      />
      <Toggle
        label={t('settings.playback.mergeM3u')}
        hint={t('settings.playback.mergeM3uHint')}
        checked={draft.MergeAllM3U}
        onChange={(value) => update({ MergeAllM3U: value })}
      />
      <Toggle
        label={t('settings.playback.fsActive')}
        hint={t('settings.playback.fsActiveHint')}
        checked={draft.ShowFSActiveTorr}
        onChange={(value) => update({ ShowFSActiveTorr: value })}
      />
      <NotApplied>
        <Toggle
          label={t('settings.playback.responsive')}
          checked={draft.ResponsiveMode}
          onChange={(value) => update({ ResponsiveMode: value })}
        />
        <Toggle
          label={t('settings.playback.debug')}
          checked={draft.EnableDebug}
          onChange={(value) => update({ EnableDebug: value })}
        />
      </NotApplied>
    </Group>
  )
}

export function NetworkSection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  const flag = (key: keyof BtSettings, label: string) => (
    <Toggle label={label} checked={draft[key] as boolean} onChange={(value) => update({ [key]: value })} />
  )
  return (
    <Group title={t('settings.network.title')} description={t('settings.network.description')}>
      <SelectInput
        label={t('settings.network.retrackers')}
        hint={t('settings.network.retrackersHint')}
        value={draft.RetrackersMode}
        options={[0, 1, 2, 3].map((value) => ({ value, label: t(`settings.network.retrackersMode.${value}`) }))}
        onChange={(value) => update({ RetrackersMode: value })}
      />
      <TextArea
        label={t('settings.network.trackers')}
        hint={t('settings.network.trackersHint')}
        value={draft.DefaultTrackers}
        rows={8}
        onChange={(value) => update({ DefaultTrackers: value })}
      />
      <div className="grid gap-5 sm:grid-cols-2">
        <NumberInput
          label={t('settings.network.download')}
          hint={t('settings.network.limitHint')}
          value={draft.DownloadRateLimit}
          min={0}
          suffix={t('settings.network.kbps')}
          onChange={(value) => update({ DownloadRateLimit: value })}
        />
        <NumberInput
          label={t('settings.network.upload')}
          hint={t('settings.network.uploadHint')}
          value={draft.UploadRateLimit}
          min={0}
          suffix={t('settings.network.kbps')}
          onChange={(value) => update({ UploadRateLimit: value })}
        />
      </div>
      <NotApplied>
        <TextInput
          label={t('settings.network.trackersUrl')}
          type="url"
          value={draft.TrackersListURL}
          onChange={(value) => update({ TrackersListURL: value })}
        />
        <div className="grid gap-5 sm:grid-cols-2">
          <NumberInput
            label={t('settings.network.connections')}
            value={draft.ConnectionsLimit}
            min={1}
            onChange={(value) => update({ ConnectionsLimit: value })}
          />
          <NumberInput
            label={t('settings.network.port')}
            hint={t('settings.network.portHint')}
            value={draft.PeersListenPort}
            min={0}
            max={65535}
            onChange={(value) => update({ PeersListenPort: value })}
          />
        </div>
        {flag('ForceEncrypt', t('settings.network.encrypt'))}
        {flag('DisableUpload', t('settings.network.noUpload'))}
        {flag('DisableDHT', t('settings.network.noDht'))}
        {flag('DisablePEX', t('settings.network.noPex'))}
        {flag('DisableTCP', t('settings.network.noTcp'))}
        {flag('DisableUTP', t('settings.network.noUtp'))}
        {flag('DisableUPNP', t('settings.network.noUpnp'))}
        {flag('EnableIPv6', t('settings.network.ipv6'))}
        {flag('EnableLPD', t('settings.network.lpd'))}
        {flag('LPDIPv6', t('settings.network.lpdIpv6'))}
        <NumberInput
          label={t('settings.network.sslPort')}
          value={draft.SslPort}
          min={0}
          max={65535}
          onChange={(value) => update({ SslPort: value })}
        />
        <TextInput label={t('settings.network.sslCert')} value={draft.SslCert} onChange={(value) => update({ SslCert: value })} />
        <TextInput label={t('settings.network.sslKey')} value={draft.SslKey} onChange={(value) => update({ SslKey: value })} />
      </NotApplied>
    </Group>
  )
}

export function DiscoverySection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  return (
    <Group title={t('settings.discovery.title')} description={t('settings.discovery.description')}>
      <Toggle
        label={t('settings.discovery.dlna')}
        hint={t('settings.discovery.dlnaHint')}
        checked={draft.EnableDLNA}
        onChange={(value) => update({ EnableDLNA: value })}
      />
      <TextInput
        label={t('settings.discovery.name')}
        hint={t('settings.discovery.nameHint')}
        value={draft.FriendlyName}
        placeholder="TorrServer"
        onChange={(value) => update({ FriendlyName: value })}
      />
      <Toggle
        label={t('settings.discovery.bonjour')}
        hint={t('settings.discovery.bonjourHint')}
        checked={draft.EnableBonjour}
        onChange={(value) => update({ EnableBonjour: value })}
      />
    </Group>
  )
}

const emptyIndexer: TorznabConfig = { Name: '', Host: '', Key: '', Categories: '', CatType: '' }

function Indexer({
  indexer,
  onChange,
  onRemove,
}: {
  indexer: TorznabConfig
  onChange: (value: TorznabConfig) => void
  onRemove: () => void
}) {
  const { t } = useTranslation()
  const test = useMutation({ mutationFn: () => testTorznab(indexer.Host, indexer.Key) })
  return (
    <li className="flex flex-col gap-4 rounded-xl border border-line p-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <TextInput label={t('settings.search.indexerName')} value={indexer.Name} onChange={(Name) => onChange({ ...indexer, Name })} />
        <TextInput
          label={t('settings.search.indexerHost')}
          type="url"
          placeholder="http://jackett:9117/api/v2.0/indexers/all/results/torznab"
          value={indexer.Host}
          onChange={(Host) => onChange({ ...indexer, Host })}
        />
        <TextInput
          label={t('settings.search.indexerKey')}
          type="password"
          value={indexer.Key}
          onChange={(Key) => onChange({ ...indexer, Key })}
        />
        <TextInput
          label={t('settings.search.indexerCategories')}
          hint={t('settings.search.indexerCategoriesHint')}
          placeholder="2000,5000"
          value={indexer.Categories}
          onChange={(Categories) => onChange({ ...indexer, Categories })}
        />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => test.mutate()} disabled={!indexer.Host || test.isPending}>
          {test.isPending ? t('settings.search.testing') : t('settings.search.test')}
        </Button>
        {test.data?.success && (
          <span className="flex items-center gap-1 text-sm text-ok">
            <CheckIcon size={16} />
            {t('settings.search.testOk')}
          </span>
        )}
        {test.data && !test.data.success && (
          <span className="text-sm text-danger">{test.data.error || t('settings.search.testFailed')}</span>
        )}
        {test.isError && <span className="text-sm text-danger">{errorText(test.error)}</span>}
        <IconButton label={t('settings.search.removeIndexer')} variant="ghost" className="ml-auto" onClick={onRemove}>
          <TrashIcon size={18} />
        </IconButton>
      </div>
    </li>
  )
}

export function SearchSection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  const indexers = draft.TorznabUrls ?? []
  const setIndexers = (next: TorznabConfig[]) => update({ TorznabUrls: next })
  return (
    <Group title={t('settings.search.title')} description={t('settings.search.description')}>
      <Toggle
        label={t('settings.search.rutor')}
        hint={t('settings.search.rutorHint')}
        checked={draft.EnableRutorSearch}
        onChange={(value) => update({ EnableRutorSearch: value })}
      />
      <Toggle
        label={t('settings.search.torznab')}
        hint={t('settings.search.torznabHint')}
        checked={draft.EnableTorznabSearch}
        onChange={(value) => update({ EnableTorznabSearch: value })}
      />
      <div className="flex flex-col gap-3">
        <h3 className="text-sm font-semibold">{t('settings.search.indexers')}</h3>
        {indexers.length === 0 && <p className="text-sm text-muted">{t('settings.search.noIndexers')}</p>}
        <ul className="flex flex-col gap-3">
          {indexers.map((indexer, index) => (
            <Indexer
              key={index}
              indexer={indexer}
              onChange={(value) => setIndexers(indexers.map((item, i) => (i === index ? value : item)))}
              onRemove={() => setIndexers(indexers.filter((_, i) => i !== index))}
            />
          ))}
        </ul>
        <Button className="w-fit" onClick={() => setIndexers([...indexers, { ...emptyIndexer }])}>
          <PlusIcon size={18} />
          {t('settings.search.addIndexer')}
        </Button>
      </div>
    </Group>
  )
}

export function TmdbSection({ draft, update }: SectionProps) {
  const { t } = useTranslation()
  const tmdb = draft.TMDBSettings
  const set = (patch: Partial<typeof tmdb>) => update({ TMDBSettings: { ...tmdb, ...patch } })
  return (
    <Group title={t('settings.tmdb.title')} description={t('settings.tmdb.description')}>
      <TextInput
        label={t('settings.tmdb.key')}
        type="password"
        value={tmdb.APIKey}
        onChange={(APIKey) => set({ APIKey })}
      />
      <TextInput label={t('settings.tmdb.apiUrl')} type="url" value={tmdb.APIURL} onChange={(APIURL) => set({ APIURL })} />
      <TextInput
        label={t('settings.tmdb.imageUrl')}
        type="url"
        value={tmdb.ImageURL}
        onChange={(ImageURL) => set({ ImageURL })}
      />
      <TextInput
        label={t('settings.tmdb.imageUrlRu')}
        type="url"
        value={tmdb.ImageURLRu}
        onChange={(ImageURLRu) => set({ ImageURLRu })}
      />
    </Group>
  )
}
