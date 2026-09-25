import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type ComponentType, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import { type BtSettings, getSettings, resetSettings, saveSettings } from '../../api/settings'
import { Button } from '../../components/ui'
import { href } from '../../lib/router'
import {
  CacheSection,
  DiscoverySection,
  NetworkSection,
  PlaybackSection,
  SearchSection,
  type SectionProps,
  TmdbSection,
} from './BtSections'
import { changedKeys } from './draft'
import { AboutSection, Capabilities, GstSection, Shutdown, StorageSection, WafSection } from './ServiceSections'

/** Sections over `BTSets` share one draft and one save; the others keep
 * their own API and save by themselves. */
const btSections: Record<string, ComponentType<SectionProps>> = {
  cache: CacheSection,
  playback: PlaybackSection,
  network: NetworkSection,
  discovery: DiscoverySection,
  search: SearchSection,
  tmdb: TmdbSection,
}
const serviceSections: Record<string, ComponentType> = {
  waf: WafSection,
  gstreamer: GstSection,
  storage: StorageSection,
  about: AboutSection,
}
export const sections = [...Object.keys(btSections), ...Object.keys(serviceSections)]

const btKey = ['settings', 'bt'] as const

function BtEditor({
  Section,
  draft,
  setDraft,
}: {
  Section: ComponentType<SectionProps>
  draft: BtSettings | null
  setDraft: (draft: BtSettings | null) => void
}) {
  const { t } = useTranslation()
  const client = useQueryClient()
  const settings = useQuery({ queryKey: btKey, queryFn: getSettings })
  const done = () => {
    setDraft(null)
    return client.invalidateQueries({ queryKey: ['settings'] })
  }
  const save = useMutation({ mutationFn: saveSettings, onSuccess: done })
  const reset = useMutation({ mutationFn: resetSettings, onSuccess: done })

  if (!settings.data) {
    return settings.isError ? (
      <p role="alert" className="text-danger">
        {t('common.error', { message: errorText(settings.error) })}
      </p>
    ) : (
      <p className="text-muted">{t('common.loading')}</p>
    )
  }
  const saved = settings.data
  const current = draft ?? saved
  const changed = draft ? changedKeys(saved, draft).length : 0
  const error = save.error ?? reset.error

  return (
    <div className="flex flex-col gap-4">
      <Section draft={current} update={(patch) => setDraft({ ...current, ...patch })} />
      {/* Pinned to the bottom only while there is something to save. */}
      <div
        className={`flex flex-col gap-3 rounded-2xl border border-line bg-surface/95 p-4 ${
          changed > 0 ? 'sticky bottom-20 z-10 shadow-lg backdrop-blur lg:bottom-4' : ''
        }`}
      >
        <div className="flex flex-wrap items-center gap-3">
          <span className="grow text-sm text-muted" role="status">
            {changed > 0
              ? t('settings.changed', { count: changed })
              : save.isSuccess || reset.isSuccess
                ? t('settings.saved')
                : t('settings.unchanged')}
          </span>
          <Button
            variant="ghost"
            disabled={reset.isPending}
            onClick={() => window.confirm(t('settings.resetConfirm')) && reset.mutate()}
          >
            {t('settings.defaults')}
          </Button>
          {changed > 0 && <Button onClick={() => setDraft(null)}>{t('settings.discard')}</Button>}
          <Button variant="primary" disabled={changed === 0 || save.isPending} onClick={() => draft && save.mutate(draft)}>
            {save.isPending ? t('settings.saving') : t('common.save')}
          </Button>
        </div>
        {changed > 0 && <p className="text-[13px] text-muted">{t('settings.reconnectNote')}</p>}
        {error && (
          <p role="alert" className="text-sm text-danger">
            {t('common.error', { message: errorText(error) })}
          </p>
        )}
      </div>
    </div>
  )
}

export function SettingsPage({ section }: { section?: string }) {
  const { t } = useTranslation()
  const active = section && sections.includes(section) ? section : 'cache'
  const BtSection = btSections[active]
  const ServiceSection = serviceSections[active]
  // Kept here so edits survive moving between groups.
  const [draft, setDraft] = useState<BtSettings | null>(null)
  const nav = useRef<HTMLElement>(null)

  // On phones the groups scroll sideways: bring the current one into view
  // without moving the page itself.
  useEffect(() => {
    const strip = nav.current
    const link = strip?.querySelector<HTMLElement>('[aria-current="page"]')
    if (strip && link && strip.scrollWidth > strip.clientWidth) {
      strip.scrollLeft = link.offsetLeft - strip.offsetLeft - 16
    }
  }, [active])

  return (
    <main className="flex flex-col gap-5 px-4 py-5 lg:px-10 lg:py-8">
      <h1 className="font-display text-3xl font-bold tracking-tight lg:text-[34px]">{t('settings.title')}</h1>
      <div className="grid gap-6 lg:grid-cols-[220px_minmax(0,1fr)] xl:grid-cols-[220px_minmax(0,1fr)_280px]">
        <nav ref={nav} aria-label={t('settings.groups')} className="-mx-4 flex gap-1 overflow-x-auto px-4 lg:mx-0 lg:flex-col lg:px-0">
          {sections.map((name) => (
            <a
              key={name}
              href={href({ name: 'settings', section: name })}
              aria-current={name === active ? 'page' : undefined}
              className={`shrink-0 rounded-[10px] px-3 py-2.5 text-[15px] no-underline ${
                name === active ? 'bg-surface font-semibold text-ink shadow-sm' : 'text-muted hover:text-ink'
              }`}
            >
              {t(`settings.nav.${name}`)}
            </a>
          ))}
        </nav>
        <div className="min-w-0">
          {BtSection && <BtEditor Section={BtSection} draft={draft} setDraft={setDraft} />}
          {ServiceSection && <ServiceSection />}
        </div>
        <aside className="hidden flex-col gap-4 xl:flex">
          <Capabilities />
          <Shutdown />
        </aside>
      </div>
    </main>
  )
}
