import { describe, expect, it } from 'vitest'

import { defaultMode, extension, formatTime, mediaKind } from './media'

describe('media', () => {
  it('classify files by extension', () => {
    expect(extension('Медиа коллекция/01 Пример/Фильм.MKV')).toBe('mkv')
    expect(extension('README')).toBe('')
    expect(mediaKind('a/b.mkv')).toBe('video')
    expect(mediaKind('clip.wav')).toBe('audio')
    expect(mediaKind('Фильм.srt')).toBeNull()
    expect(mediaKind('Фильм.ac3')).toBeNull()
  })

  it('prefer the browser, then HLS for video', () => {
    const chrome = (mime: string) => mime === 'video/mp4' || mime === 'audio/wav'
    expect(defaultMode('movie.mp4', true, chrome)).toBe('direct')
    expect(defaultMode('movie.mkv', true, chrome)).toBe('hls')
    expect(defaultMode('movie.mkv', false, chrome)).toBe('direct')
    expect(defaultMode('clip.wav', true, chrome)).toBe('direct')
  })

  it('format playback time', () => {
    expect(formatTime(0)).toBe('0:00')
    expect(formatTime(222.9)).toBe('3:42')
    expect(formatTime(3725)).toBe('1:02:05')
    expect(formatTime(Number.NaN)).toBe('0:00')
  })
})
