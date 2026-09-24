import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'

import { getCache } from '../../api/cache'
import { useLanguage } from '../../i18n'
import { formatBytes, formatPercent } from '../../lib/format'
import { type CellState, cacheCells } from './cacheCells'

const colors: Record<CellState, string> = {
  cached: 'bg-ok',
  partial: 'bg-ok/45',
  reading: 'bg-accent',
  empty: 'bg-line',
}

/** Which pieces the cache holds and where readers are, refreshed every two
 * seconds while the torrent is loaded. */
export function CacheMap({ hash }: { hash: string }) {
  const { t } = useTranslation()
  const language = useLanguage()
  const cache = useQuery({
    queryKey: ['cache', hash],
    queryFn: () => getCache(hash),
    refetchInterval: 2_000,
    retry: false,
  })
  if (!cache.data) return null
  const state = cache.data
  const cells = cacheCells(state)
  const perCell = cells[0] ? cells[0].to - cells[0].from + 1 : 1

  return (
    <section aria-label={t('cache.title')} className="flex flex-col gap-3 rounded-2xl border border-line bg-surface p-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <h2 className="grow font-display text-xl font-bold">{t('cache.title')}</h2>
        <span className="font-mono text-sm text-muted">
          {formatBytes(state.Filled, language)} / {formatBytes(state.Capacity, language)} ·{' '}
          {state.Filled > 0 && state.Filled * 100 < state.Capacity ? '< 1 %' : formatPercent(state.Filled, state.Capacity)}
        </span>
      </div>
      <p className="text-[13px] text-muted">
        {t('cache.pieces', { count: state.PiecesCount, size: formatBytes(state.PiecesLength, language) })}
        {perCell > 1 && ` · ${t('cache.perCell', { count: perCell })}`}
      </p>
      <div role="img" aria-label={t('cache.mapLabel')} className="flex flex-wrap gap-[3px]">
        {cells.map((cell) => (
          <span
            key={cell.from}
            title={`${cell.from === cell.to ? cell.from : `${cell.from}–${cell.to}`}: ${Math.round(cell.filled * 100)} %`}
            className={`size-2.5 rounded-[2px] ${colors[cell.state]}`}
          />
        ))}
      </div>
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-[13px] text-muted">
        {(['cached', 'partial', 'reading', 'empty'] as const).map((key) => (
          <span key={key} className="flex items-center gap-1.5">
            <span className={`size-2.5 rounded-[2px] ${colors[key]}`} />
            {t(`cache.${key}`)}
          </span>
        ))}
      </div>
    </section>
  )
}
