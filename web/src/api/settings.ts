import { getText, postJson, request } from './http'

export interface TorznabConfig {
  Host: string
  Key: string
  Name: string
  Categories: string
  CatType: string
}

export interface TmdbConfig {
  APIKey: string
  APIURL: string
  ImageURL: string
  ImageURLRu: string
}

/** `BTSets`, field for field. */
export interface BtSettings {
  CacheSize: number
  ReaderReadAHead: number
  PreloadCache: number
  UseDisk: boolean
  TorrentsSavePath: string
  RemoveCacheOnDrop: boolean
  ForceEncrypt: boolean
  RetrackersMode: number
  TrackersListURL: string
  DefaultTrackers: string
  TorrentDisconnectTimeout: number
  EnableDebug: boolean
  EnableDLNA: boolean
  EnableBonjour: boolean
  FriendlyName: string
  EnableRutorSearch: boolean
  EnableTorznabSearch: boolean
  TorznabUrls: TorznabConfig[] | null
  TMDBSettings: TmdbConfig
  EnableIPv6: boolean
  DisableTCP: boolean
  DisableUTP: boolean
  DisableUPNP: boolean
  DisableDHT: boolean
  DisablePEX: boolean
  DisableUpload: boolean
  DownloadRateLimit: number
  UploadRateLimit: number
  ConnectionsLimit: number
  PeersListenPort: number
  EnableLPD: boolean
  LPDIPv6: boolean
  SslPort: number
  SslCert: string
  SslKey: string
  ResponsiveMode: boolean
  ShowFSActiveTorr: boolean
  StoreSettingsInJson: boolean
  StoreViewedInJson: boolean
  TrackTimecode: boolean
  MergeAllM3U: boolean
}

export function getSettings(): Promise<BtSettings> {
  return postJson<BtSettings>('/settings', { action: 'get' })
}

/** Applies the whole set; the server reconnects its BitTorrent client, which
 * unloads every torrent nobody is reading. */
export async function saveSettings(sets: BtSettings): Promise<void> {
  await postJson('/settings', { action: 'set', sets })
}

export async function resetSettings(): Promise<void> {
  await postJson('/settings', { action: 'def' })
}

export type StorageKind = 'json' | 'bbolt'

export interface StorageSettings {
  settings: StorageKind
  viewed: StorageKind
  viewedCount: number
}

export async function getStorage(): Promise<StorageSettings> {
  return (await request('/storage/settings')).json() as Promise<StorageSettings>
}

export async function saveStorage(choice: { settings: StorageKind; viewed: StorageKind }): Promise<void> {
  await postJson('/storage/settings', choice)
}

export interface WafLists {
  whitelist: string
  blacklist: string
  referers: string
}

export interface WafWarning {
  list: string
  line?: number
  code: string
}

export interface WafState extends WafLists {
  ip_enabled: boolean
  referer_enabled: boolean
  read_only: boolean
  warnings: WafWarning[]
}

export async function getWaf(): Promise<WafState> {
  return (await request('/waf')).json() as Promise<WafState>
}

export function saveWaf(lists: WafLists): Promise<WafState> {
  return postJson<WafState>('/waf', lists)
}

/** Asks the indexer for its capabilities with this key. */
export async function testTorznab(host: string, key: string): Promise<{ success: boolean; error?: string }> {
  return postJson('/torznab/test', { host, key })
}

/** The GStreamer module's own settings, stored apart from `BTSets`. */
export interface GstConfig {
  GSTVersion: number
  GSTPath: string
  Source: string
  MaxTasks: number
  InactiveMinutes: number
  AACBitrateKbps: number
  AACChannels: number
  AACSamplerate: number
  SegmentSeconds: number
  SegmentDiff: number
  Subtitles: boolean
  TranscodeH264: boolean
  TranscodeH265: boolean
  TranscodeAV1: boolean
  TranscodeVP9: boolean
  TranscodeVP8: boolean
  TranscodeAVI: boolean
  HDRToSDR: boolean
  HardwareAcceleration: boolean
  UseGPU: boolean
  X264Ultrafast: boolean
  VideoBitrate: number
}

export type GstSettings = { built_in: false } | { built_in: true; config: GstConfig; defaults: GstConfig }

export async function getGstSettings(): Promise<GstSettings> {
  return JSON.parse(await getText('/gst/settings')) as GstSettings
}

export async function saveGstSettings(config: GstConfig): Promise<void> {
  await postJson('/gst/settings', { action: 'set', config })
}

export async function resetGstSettings(): Promise<void> {
  await postJson('/gst/settings', { action: 'def' })
}

export interface ComponentStatus {
  found: boolean
  available: boolean
  works: boolean
  version?: string
  error?: string
}

export interface GstEcho {
  gst_discoverer: ComponentStatus
  gstreamer: ComponentStatus
  hdr_tone_mapping: ComponentStatus
  embedded_runtime: ComponentStatus
}

export async function gstEcho(): Promise<GstEcho> {
  return (await request('/gst/echo')).json() as Promise<GstEcho>
}

export async function ffprobeAvailable(): Promise<boolean> {
  const status = (await (await request('/ffp/status')).json()) as { available?: boolean }
  return status.available === true
}

export async function shutdownServer(): Promise<void> {
  await request('/shutdown')
}
