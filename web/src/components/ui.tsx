import { type ButtonHTMLAttributes, type ReactNode, useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'

import { CloseIcon } from './icons'

type Variant = 'primary' | 'secondary' | 'danger' | 'ghost'

const variants: Record<Variant, string> = {
  primary: 'border-transparent bg-accent text-on-accent font-semibold hover:brightness-110',
  secondary: 'border-line bg-surface text-ink hover:bg-raised',
  danger: 'border-danger/40 bg-surface text-danger hover:bg-danger/10',
  ghost: 'border-transparent bg-transparent text-muted hover:bg-raised hover:text-ink',
}

export function Button({
  variant = 'secondary',
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      type="button"
      className={`inline-flex h-11 items-center justify-center gap-2 rounded-[10px] border px-4 text-[15px] transition disabled:cursor-not-allowed disabled:opacity-50 ${variants[variant]} ${className}`}
      {...props}
    />
  )
}

/** A square button with an icon; `label` is its accessible name. */
export function IconButton({
  label,
  variant = 'secondary',
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; variant?: Variant }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={`inline-flex size-10 shrink-0 items-center justify-center rounded-lg border transition disabled:opacity-50 ${variants[variant]} ${className}`}
      {...props}
    />
  )
}

export function Chip({ active, children, onClick }: { active: boolean; children: ReactNode; onClick: () => void }) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`h-9 shrink-0 rounded-full border px-3.5 text-sm transition ${
        active ? 'border-accent bg-accent-soft font-medium text-accent' : 'border-line text-muted hover:text-ink'
      }`}
    >
      {children}
    </button>
  )
}

export function StatusDot({ tone }: { tone: 'ok' | 'warn' | 'idle' }) {
  const color = tone === 'ok' ? 'bg-ok' : tone === 'warn' ? 'bg-warn' : 'bg-muted'
  return <span aria-hidden="true" className={`inline-block size-2 shrink-0 rounded-full ${color}`} />
}

/** A modal on the native `<dialog>`: focus trapping, Escape and the backdrop
 * come from the browser. */
export function Dialog({
  open,
  title,
  onClose,
  children,
  wide = false,
}: {
  open: boolean
  title: string
  onClose: () => void
  children: ReactNode
  wide?: boolean
}) {
  const { t } = useTranslation()
  const ref = useRef<HTMLDialogElement>(null)
  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    if (open && !dialog.open) dialog.showModal()
    if (!open && dialog.open) dialog.close()
  }, [open])
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="dialog-title"
      className={`m-auto max-h-[92dvh] w-[calc(100%-2rem)] ${wide ? 'max-w-4xl' : 'max-w-xl'} rounded-2xl border border-line bg-surface p-0 text-ink backdrop:bg-black/50`}
    >
      {open && (
        <div className="flex flex-col gap-5 p-6 sm:p-8">
          <div className="flex items-center gap-4">
            <h2 id="dialog-title" className="grow font-display text-2xl font-bold tracking-tight">
              {title}
            </h2>
            <IconButton label={t('common.close')} onClick={onClose} className="size-11">
              <CloseIcon />
            </IconButton>
          </div>
          {children}
        </div>
      )}
    </dialog>
  )
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-2 text-sm font-semibold">
      {label}
      {children}
      {hint && <span className="text-[13px] font-normal text-muted">{hint}</span>}
    </label>
  )
}

export const inputClass =
  'h-11 w-full rounded-[10px] border border-line bg-surface px-3 text-[15px] font-normal text-ink placeholder:text-muted focus:outline-2 focus:outline-accent'
