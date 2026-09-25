import { type FormEvent, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { errorText } from '../../api/http'
import type { AddOptions } from '../../api/torrents'
import { UploadIcon } from '../../components/icons'
import { Button, Dialog, Field, inputClass } from '../../components/ui'
import { useAddLink, useUpload } from './queries'
import { categories } from './status'

type Tab = 'link' | 'files'

export function AddDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useTranslation()
  const [tab, setTab] = useState<Tab>('link')
  const [link, setLink] = useState('')
  const [title, setTitle] = useState('')
  const [poster, setPoster] = useState('')
  const [category, setCategory] = useState('')
  const [save, setSave] = useState(true)
  const [files, setFiles] = useState<File[]>([])
  const [dragging, setDragging] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  const addLink = useAddLink()
  const upload = useUpload()
  const pending = addLink.isPending || upload.isPending
  const error = addLink.error ?? upload.error

  function reset() {
    setLink('')
    setTitle('')
    setPoster('')
    setCategory('')
    setFiles([])
    addLink.reset()
    upload.reset()
  }

  function close() {
    reset()
    onClose()
  }

  async function submit(event: FormEvent) {
    event.preventDefault()
    const options: AddOptions = { title: title.trim(), poster: poster.trim(), category, save }
    if (tab === 'link') await addLink.mutateAsync({ link: link.trim(), options })
    else await upload.mutateAsync({ files, options })
    close()
  }

  const ready = tab === 'link' ? link.trim() !== '' : files.length > 0
  const tabClass = (value: Tab) =>
    `h-10 rounded-[9px] px-4 text-[15px] ${tab === value ? 'bg-surface font-semibold text-ink shadow-sm' : 'text-muted'}`

  return (
    <Dialog open={open} title={t('add.title')} onClose={close}>
      <form onSubmit={(event) => void submit(event).catch(() => undefined)} className="flex flex-col gap-5">
        <div role="tablist" className="flex gap-1 self-start rounded-xl bg-raised p-1">
          <button type="button" role="tab" aria-selected={tab === 'link'} className={tabClass('link')} onClick={() => setTab('link')}>
            {t('add.tabLink')}
          </button>
          <button type="button" role="tab" aria-selected={tab === 'files'} className={tabClass('files')} onClick={() => setTab('files')}>
            {t('add.tabFiles')}
          </button>
        </div>
        {tab === 'link' ? (
          <Field label={t('add.link')}>
            <textarea
              value={link}
              onChange={(event) => setLink(event.target.value)}
              rows={3}
              required
              className={`${inputClass} h-auto py-2.5 font-mono text-sm`}
              placeholder="magnet:?xt=urn:btih:…"
            />
          </Field>
        ) : (
          <div
            onDragOver={(event) => {
              event.preventDefault()
              setDragging(true)
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(event) => {
              event.preventDefault()
              setDragging(false)
              setFiles([...event.dataTransfer.files].filter((file) => file.name.endsWith('.torrent')))
            }}
            className={`flex flex-col items-center gap-3 rounded-xl border-2 border-dashed p-8 text-center ${
              dragging ? 'border-accent bg-accent-soft' : 'border-line'
            }`}
          >
            <UploadIcon size={28} className="text-muted" />
            <span className="text-muted">{files.length ? t('add.chosen', { count: files.length }) : t('add.drop')}</span>
            <input
              ref={input}
              type="file"
              accept=".torrent,application/x-bittorrent"
              multiple
              className="sr-only"
              onChange={(event) => setFiles([...(event.target.files ?? [])])}
            />
            <Button onClick={() => input.current?.click()}>{t('add.choose')}</Button>
          </div>
        )}
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label={t('add.name')} hint={t('add.nameHint')}>
            <input value={title} onChange={(event) => setTitle(event.target.value)} className={inputClass} />
          </Field>
          <Field label={t('add.category')}>
            <select value={category} onChange={(event) => setCategory(event.target.value)} className={inputClass}>
              <option value="">{t('category.none')}</option>
              {categories.map((item) => (
                <option key={item} value={item}>
                  {t(`category.${item}`)}
                </option>
              ))}
            </select>
          </Field>
          <Field label={t('add.poster')}>
            <input type="url" value={poster} onChange={(event) => setPoster(event.target.value)} className={inputClass} placeholder="https://" />
          </Field>
          <label className="flex items-start gap-3 self-end pb-2 text-[15px]">
            <input type="checkbox" checked={save} onChange={(event) => setSave(event.target.checked)} className="mt-1 size-5 accent-accent" />
            <span className="flex flex-col">
              {t('add.save')}
              <span className="text-[13px] text-muted">{t('add.saveHint')}</span>
            </span>
          </label>
        </div>
        {error && (
          <p role="alert" className="rounded-lg bg-danger/10 px-3 py-2 text-sm text-danger">
            {t('common.error', { message: errorText(error) })}
          </p>
        )}
        <div className="flex justify-end gap-3">
          <Button onClick={close}>{t('common.cancel')}</Button>
          <Button type="submit" variant="primary" disabled={!ready || pending}>
            {pending ? t('add.adding') : t('add.submit')}
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
