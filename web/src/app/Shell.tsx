import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'

import { serverVersion } from '../api/server'
import { ListIcon, LogoIcon, MonitorIcon, MoonIcon, SearchIcon, SettingsIcon, SunIcon } from '../components/icons'
import { StatusDot } from '../components/ui'
import { languages, setLanguage, useLanguage } from '../i18n'
import { href, type Route } from '../lib/router'
import { type ThemeChoice, useTheme } from '../lib/theme'

const sections = [
  { route: { name: 'torrents' }, key: 'nav.torrents', Icon: ListIcon },
  { route: { name: 'search' }, key: 'nav.search', Icon: SearchIcon },
  { route: { name: 'settings' }, key: 'nav.settings', Icon: SettingsIcon },
] as const

function activeSection(route: Route): Route['name'] {
  return route.name === 'torrent' ? 'torrents' : route.name
}

const nextTheme: Record<ThemeChoice, ThemeChoice> = { system: 'light', light: 'dark', dark: 'system' }
const themeIcon: Record<ThemeChoice, typeof SunIcon> = { system: MonitorIcon, light: SunIcon, dark: MoonIcon }

function Preferences() {
  const { t } = useTranslation()
  const language = useLanguage()
  const { choice, choose } = useTheme()
  const ThemeIcon = themeIcon[choice]
  const other = languages.find((item) => item !== language) ?? 'ru'
  return (
    <div className="flex gap-2">
      <button
        type="button"
        onClick={() => void setLanguage(other)}
        aria-label={t('language.switch')}
        className="h-9 grow rounded-lg border border-line bg-surface text-[13px] text-ink hover:bg-raised"
      >
        {language.toUpperCase()} · {other.toUpperCase()}
      </button>
      <button
        type="button"
        onClick={() => choose(nextTheme[choice])}
        aria-label={`${t('theme.label')}: ${t(`theme.${choice}`)}`}
        title={`${t('theme.label')}: ${t(`theme.${choice}`)}`}
        className="flex h-9 w-11 items-center justify-center rounded-lg border border-line bg-surface text-ink hover:bg-raised"
      >
        <ThemeIcon size={18} />
      </button>
    </div>
  )
}

function ServerStatus() {
  const { t } = useTranslation()
  const version = useQuery({ queryKey: ['server', 'version'], queryFn: serverVersion, refetchInterval: 30_000 })
  return (
    <div className="flex items-center gap-2 text-[13px] text-muted">
      <StatusDot tone={version.isSuccess ? 'ok' : version.isError ? 'warn' : 'idle'} />
      <span>
        {version.isSuccess
          ? t('app.server', { version: version.data })
          : version.isError
            ? t('app.unreachable')
            : t('app.connecting')}
      </span>
    </div>
  )
}

export function Shell({ route, children }: { route: Route; children: ReactNode }) {
  const { t } = useTranslation()
  const active = activeSection(route)
  return (
    <div className="flex min-h-dvh bg-ground text-ink">
      <aside className="sticky top-0 hidden h-dvh w-62 shrink-0 flex-col gap-7 border-r border-line bg-raised px-5 py-7 lg:flex">
        <a href={href({ name: 'torrents' })} className="flex items-center gap-2.5 px-2 text-ink no-underline">
          <LogoIcon size={30} className="text-accent" />
          <span className="font-display text-2xl font-bold tracking-tight">{t('app.title')}</span>
        </a>
        <nav aria-label={t('nav.sections')} className="flex flex-col gap-1">
          {sections.map(({ route: target, key, Icon }) => (
            <a
              key={key}
              href={href(target)}
              aria-current={active === target.name ? 'page' : undefined}
              className={`flex items-center gap-3 rounded-[10px] px-3 py-2.5 no-underline ${
                active === target.name ? 'bg-surface font-semibold text-ink' : 'text-muted hover:text-ink'
              }`}
            >
              <Icon />
              {t(key)}
            </a>
          ))}
        </nav>
        <div className="mt-auto flex flex-col gap-3 rounded-xl border border-line p-3.5">
          <ServerStatus />
          <Preferences />
        </div>
      </aside>
      <div className="flex min-w-0 grow flex-col pb-20 lg:pb-0">
        <header className="flex items-center gap-2.5 px-4 pt-5 lg:hidden">
          <LogoIcon size={28} className="text-accent" />
          <span className="grow font-display text-xl font-bold tracking-tight">{t('app.title')}</span>
          <div className="w-40">
            <Preferences />
          </div>
        </header>
        {children}
      </div>
      <nav
        aria-label={t('nav.sections')}
        className="fixed inset-x-0 bottom-0 grid grid-cols-3 border-t border-line bg-surface px-2 pt-2 pb-[max(env(safe-area-inset-bottom),0.75rem)] lg:hidden"
      >
        {sections.map(({ route: target, key, Icon }) => (
          <a
            key={key}
            href={href(target)}
            aria-current={active === target.name ? 'page' : undefined}
            className={`flex flex-col items-center gap-1 p-1.5 text-xs no-underline ${
              active === target.name ? 'font-semibold text-accent' : 'text-muted'
            }`}
          >
            <Icon size={22} />
            {t(key)}
          </a>
        ))}
      </nav>
    </div>
  )
}
