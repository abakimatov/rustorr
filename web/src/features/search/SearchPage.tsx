import { useQuery } from '@tanstack/react-query'
import { type FormEvent, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import { search, SearchDisabled, type SearchResult, type SearchSource } from '../../api/search'
import { getSettings } from '../../api/settings'
import { CheckIcon, PlusIcon, SearchIcon } from '../../components/icons'
import { Button } from '../../components/ui'
import { useLanguage } from '../../i18n'
import { formatBytes } from '../../lib/format'
import { href } from '../../lib/router'
import { useAddLink, useTorrents } from '../torrents/queries'
import { addLink, categoryFor, parseSize, type SortKey, sortResults } from './results'

interface Request {
  source: SearchSource
  query: string
  indexer: number
}

function Result({ result, known, save }: { result: SearchResult; known: boolean; save: boolean }) {
  const { t } = useTranslation()
  const language = useLanguage()
  const add = useAddLink()
  const category = categoryFor(result.Categories)
  const bytes = parseSize(result.Size)
  const date = Date.parse(result.CreateDate)
  const added = add.data?.hash ?? (known ? result.Hash.toLowerCase() : undefined)
  const meta = [
    category ? t(`category.${category}`) : result.Categories,
    result.Year > 0 ? String(result.Year) : '',
    Number.isFinite(date) ? new Intl.DateTimeFormat(language, { dateStyle: 'medium' }).format(date) : '',
    result.Tracker,
  ].filter(Boolean)

  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-2 border-t border-line px-4 py-3.5 first:border-t-0 md:grid-cols-[minmax(0,1fr)_110px_120px_150px] md:px-5">
      <div className="flex min-w-0 flex-col gap-1">
        <span className="text-[15px] font-semibold break-words">{result.Title}</span>
        <span className="text-[13px] text-muted">{meta.join(' · ')}</span>
        <span className="flex gap-3 font-mono text-[13px] text-muted md:hidden">
          <span>{bytes ? formatBytes(bytes, language) : result.Size}</span>
          <span className="text-ok">↑ {result.Seed}</span>
        </span>
      </div>
      <span className="hidden text-right font-mono text-sm md:block">{bytes ? formatBytes(bytes, language) : result.Size}</span>
      <span className="hidden text-right font-mono text-sm md:block">
        <span className="text-ok">{result.Seed}</span>
        <span className="text-muted"> / {result.Peer}</span>
      </span>
      <div className="flex flex-col items-end gap-1">
        {added ? (
          <a
            href={href({ name: 'torrent', hash: added })}
            className="inline-flex h-10 items-center gap-1.5 rounded-lg px-3 text-sm font-semibold text-ok no-underline hover:bg-raised"
          >
            <CheckIcon size={16} />
            {known && !add.data ? t('search.inList') : t('search.added')}
          </a>
        ) : (
          <Button
            className="h-10"
            disabled={add.isPending || !addLink(result)}
            onClick={() =>
              add.mutate({ link: addLink(result), options: { title: result.Title, category, save } })
            }
          >
            <PlusIcon size={16} />
            {add.isPending ? t('add.adding') : t('search.add')}
          </Button>
        )}
        {add.isError && <span className="text-[13px] text-danger">{errorText(add.error)}</span>}
      </div>
    </li>
  )
}

export function SearchPage() {
  const { t } = useTranslation()
  const settings = useQuery({ queryKey: ['settings', 'bt'], queryFn: getSettings })
  const torrents = useTorrents()
  const [text, setText] = useState('')
  const [chosenSource, setSource] = useState<SearchSource | null>(null)
  const [indexer, setIndexer] = useState(-1)
  const [sort, setSort] = useState<SortKey>('seeds')
  const [save, setSave] = useState(true)
  const [request, setRequest] = useState<Request | null>(null)

  const rutorOn = settings.data?.EnableRutorSearch === true
  const torznabOn = settings.data?.EnableTorznabSearch === true
  const source: SearchSource = chosenSource ?? (rutorOn || !torznabOn ? 'rutor' : 'torznab')
  const sourceOn = source === 'rutor' ? rutorOn : torznabOn
  const indexers = settings.data?.TorznabUrls ?? []

  const results = useQuery({
    queryKey: ['search', request],
    queryFn: () => (request ? search(request.source, request.query, request.indexer) : Promise.resolve([])),
    enabled: request !== null,
    staleTime: 60_000,
    retry: false,
  })
  const sorted = useMemo(() => sortResults(results.data ?? [], sort), [results.data, sort])
  const known = useMemo(() => new Set((torrents.data ?? []).map((torrent) => torrent.hash.toLowerCase())), [torrents.data])

  function submit(event: FormEvent) {
    event.preventDefault()
    const query = text.trim()
    if (query) setRequest({ source, query, indexer: source === 'torznab' ? indexer : -1 })
  }

  const disabledNotice = (
    <div className="flex flex-wrap items-center gap-3 rounded-2xl border border-dashed border-line p-5">
      <p className="grow text-muted">{t('search.disabled', { source: source === 'rutor' ? 'Rutor' : 'Torznab' })}</p>
      <a href={href({ name: 'settings', section: 'search' })} className="text-sm font-semibold text-accent no-underline">
        {t('search.openSettings')}
      </a>
    </div>
  )

  return (
    <main className="flex flex-col gap-5 px-4 py-5 lg:gap-6 lg:px-10 lg:py-8">
      <h1 className="font-display text-3xl font-bold tracking-tight lg:text-[34px]">{t('search.title')}</h1>
      <form onSubmit={submit} className="flex flex-col gap-3" role="search">
        <div className="flex gap-2">
          <label className="flex h-12 min-w-0 grow items-center gap-2.5 rounded-[10px] border border-line bg-surface px-3.5 text-muted focus-within:outline-2 focus-within:outline-accent">
            <SearchIcon size={18} />
            <span className="sr-only">{t('search.query')}</span>
            <input
              type="search"
              value={text}
              onChange={(event) => setText(event.target.value)}
              placeholder={t('search.placeholder')}
              className="h-full grow bg-transparent text-base text-ink outline-none placeholder:text-muted"
            />
          </label>
          <Button type="submit" variant="primary" className="h-12 shrink-0 px-4 sm:px-6" disabled={!text.trim() || !sourceOn}>
            {t('search.submit')}
          </Button>
        </div>
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <div role="radiogroup" aria-label={t('search.source')} className="flex rounded-[10px] border border-line bg-surface p-0.5">
            {(['rutor', 'torznab'] as const).map((value) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={source === value}
                onClick={() => setSource(value)}
                className={`h-9 rounded-lg px-3 ${source === value ? 'bg-accent-soft font-semibold text-accent' : 'text-muted hover:text-ink'}`}
              >
                {value === 'rutor' ? 'Rutor' : 'Torznab'}
              </button>
            ))}
          </div>
          {source === 'torznab' && indexers.length > 1 && (
            <label className="flex items-center gap-2 text-muted">
              {t('search.indexer')}
              <select
                value={indexer}
                onChange={(event) => setIndexer(Number(event.target.value))}
                className="h-9 max-w-56 rounded-lg border border-line bg-surface px-2 text-ink"
              >
                <option value={-1}>{t('search.allIndexers')}</option>
                {indexers.map((item, index) => (
                  <option key={index} value={index}>
                    {item.Name || item.Host}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="flex items-center gap-2 text-muted">
            {t('search.sort')}
            <select
              value={sort}
              onChange={(event) => setSort(event.target.value as SortKey)}
              className="h-9 rounded-lg border border-line bg-surface px-2 text-ink"
            >
              <option value="seeds">{t('search.bySeeds')}</option>
              <option value="size">{t('search.bySize')}</option>
              <option value="date">{t('search.byDate')}</option>
            </select>
          </label>
          <label className="flex items-center gap-2 text-muted md:ml-auto">
            <input type="checkbox" checked={save} onChange={(event) => setSave(event.target.checked)} className="size-4 accent-(--color-accent)" />
            {t('add.save')}
          </label>
        </div>
      </form>

      {settings.data && !sourceOn ? (
        disabledNotice
      ) : results.isError ? (
        results.error instanceof SearchDisabled ? (
          disabledNotice
        ) : (
          <p role="alert" className="text-danger">
            {t('common.error', { message: errorText(results.error) })}
          </p>
        )
      ) : request === null ? (
        <p className="text-muted">{t('search.hint')}</p>
      ) : results.isFetching && !results.data ? (
        <p className="text-muted">{t('search.searching')}</p>
      ) : sorted.length === 0 ? (
        <p className="rounded-2xl border border-dashed border-line px-6 py-12 text-center font-display text-xl font-bold">
          {t('search.nothing')}
        </p>
      ) : (
        <div className="overflow-hidden rounded-2xl border border-line bg-surface">
          <div className="hidden grid-cols-[minmax(0,1fr)_110px_120px_150px] gap-4 bg-raised px-5 py-3 text-xs font-semibold tracking-wider text-muted uppercase md:grid">
            <span>{t('search.found', { count: sorted.length })}</span>
            <span className="text-right">{t('torrents.columns.size')}</span>
            <span className="text-right">{t('search.seeds')}</span>
            <span />
          </div>
          <ul>
            {sorted.map((result, index) => (
              <Result
                key={`${result.Hash || result.Link}-${index}`}
                result={result}
                known={Boolean(result.Hash) && known.has(result.Hash.toLowerCase())}
                save={save}
              />
            ))}
          </ul>
        </div>
      )}
    </main>
  )
}
