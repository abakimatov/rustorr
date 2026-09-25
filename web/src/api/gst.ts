import { getText, request } from './http'

/** One stream of `gst-discoverer`'s report, as `/gst/<hash>/probe` gives it. */
export interface ProbeTrack {
  Index: number
  Type: string
  Codec: string
  Title: string
  Language: string
  Channels: number
  Width: number
  Height: number
}

export interface Probe {
  DurationNS: number
  Container: string
  Tracks: ProbeTrack[] | null
}

/** Whether the server was built with the GStreamer HLS module. */
export async function gstBuiltIn(): Promise<boolean> {
  const settings = JSON.parse(await getText('/gst/settings')) as { built_in?: boolean }
  return settings.built_in === true
}

export async function probe(hash: string, fileId: number): Promise<Probe> {
  return (await request(`/gst/${hash}/probe?index=${fileId}`)).json() as Promise<Probe>
}

/** The HLS master playlist of one file with one audio track (`Index` of the
 * probe; the server falls back to the first audio track). */
export function masterUrl(hash: string, fileId: number, audio: number): string {
  return `/gst/${hash}/master.m3u8?index=${fileId}&audio=${audio}`
}

/** Keeps the file's task from being frozen while the player is paused;
 * segment requests do the same while it plays. */
export async function heartbeat(hash: string): Promise<void> {
  await request(`/gst/${hash}/heartbeat`)
}

export function audioTracks(probe: Probe | undefined): ProbeTrack[] {
  return (probe?.Tracks ?? []).filter((track) => track.Type === 'audio')
}
