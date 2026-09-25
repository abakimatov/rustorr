/** What the browser gets for a file: its own decoder over `/play`, or the
 * server's HLS repackaging over `/gst`. */
export type Mode = 'direct' | 'hls'

const DIRECT: Record<string, string> = {
  mp4: 'video/mp4',
  m4v: 'video/mp4',
  mov: 'video/mp4',
  webm: 'video/webm',
  ogv: 'video/ogg',
  mp3: 'audio/mpeg',
  m4a: 'audio/mp4',
  aac: 'audio/aac',
  flac: 'audio/flac',
  ogg: 'audio/ogg',
  oga: 'audio/ogg',
  opus: 'audio/ogg; codecs=opus',
  wav: 'audio/wav',
}

const VIDEO = new Set(['mp4', 'm4v', 'mov', 'webm', 'ogv', 'mkv', 'avi', 'ts', 'm2ts', 'mts', 'mpg', 'mpeg', 'wmv', 'flv', '3gp', 'vob'])
// AC3 and DTS are left out: browsers do not decode them.
const AUDIO = new Set(['mp3', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'opus', 'wav', 'mka'])

export function extension(path: string): string {
  const name = path.split('/').pop() ?? path
  const dot = name.lastIndexOf('.')
  return dot < 0 ? '' : name.slice(dot + 1).toLowerCase()
}

export function mediaKind(path: string): 'video' | 'audio' | null {
  const ext = extension(path)
  if (VIDEO.has(ext)) return 'video'
  if (AUDIO.has(ext)) return 'audio'
  return null
}

/** The MIME type to ask `canPlayType` about, for formats browsers may decode. */
export function directMime(path: string): string | undefined {
  return DIRECT[extension(path)]
}

/** What the server's HLS carries: fMP4 with H.264 and AAC. Open-source
 * Chromium builds decode neither. */
export const HLS_CODECS = 'video/mp4; codecs="avc1.4d401f,mp4a.40.2"'

/** Direct when the browser says it can decode the container; otherwise HLS
 * for video when the server has it and the browser decodes its codecs;
 * otherwise direct anyway (Chromium plays many Matroska files it does not
 * admit to). */
export function defaultMode(path: string, hls: boolean, canPlay: (mime: string) => boolean): Mode {
  const mime = directMime(path)
  if (mime && canPlay(mime)) return 'direct'
  return hls && mediaKind(path) === 'video' ? 'hls' : 'direct'
}

/** Whether this browser can play the server's HLS, through MSE or natively. */
export function canPlayHls(): boolean {
  const mediaSource = (window as { ManagedMediaSource?: typeof MediaSource }).ManagedMediaSource ?? window.MediaSource
  if (mediaSource?.isTypeSupported(HLS_CODECS)) return true
  return document.createElement('video').canPlayType('application/vnd.apple.mpegurl') !== ''
}

/** `h:mm:ss` or `m:ss`. */
export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00'
  const whole = Math.floor(seconds)
  const h = Math.floor(whole / 3600)
  const m = Math.floor((whole % 3600) / 60)
  const s = String(whole % 60).padStart(2, '0')
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
}
