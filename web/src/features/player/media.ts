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
const AUDIO = new Set(['mp3', 'm4a', 'aac', 'flac', 'ogg', 'oga', 'opus', 'wav', 'ac3', 'dts', 'mka'])

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

/** Direct when the browser says it can decode the container; otherwise HLS
 * for video when the server has it; otherwise direct anyway (Chromium plays
 * many Matroska files it does not admit to). */
export function defaultMode(path: string, gst: boolean, canPlay: (mime: string) => boolean): Mode {
  const mime = directMime(path)
  if (mime && canPlay(mime)) return 'direct'
  return gst && mediaKind(path) === 'video' ? 'hls' : 'direct'
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
