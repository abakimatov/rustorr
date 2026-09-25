import { type ReactNode, useId } from 'react'
import { useTranslation } from 'react-i18next'

import { inputClass } from '../../components/ui'

export function Group({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-5 rounded-2xl border border-line bg-surface p-5 md:p-6">
      <div className="flex flex-col gap-1">
        <h2 className="font-display text-xl font-bold">{title}</h2>
        {description && <p className="text-sm text-muted">{description}</p>}
      </div>
      {children}
    </section>
  )
}

function Label({ id, label, hint, children }: { id: string; label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <label htmlFor={id} className="text-sm font-semibold">
        {label}
      </label>
      {children}
      {hint && <span className="text-[13px] text-muted">{hint}</span>}
    </div>
  )
}

export function Toggle({
  label,
  hint,
  checked,
  onChange,
  disabled,
}: {
  label: string
  hint?: string
  checked: boolean
  onChange: (value: boolean) => void
  disabled?: boolean
}) {
  const id = useId()
  return (
    <div className="flex items-start gap-4">
      <label htmlFor={id} className="flex grow flex-col gap-0.5">
        <span className="text-[15px] font-medium">{label}</span>
        {hint && <span className="text-[13px] text-muted">{hint}</span>}
      </label>
      <input
        id={id}
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        className="relative mt-0.5 h-6 w-11 shrink-0 cursor-pointer appearance-none rounded-full bg-line transition before:absolute before:top-0.5 before:left-0.5 before:size-5 before:rounded-full before:bg-surface before:shadow before:transition checked:bg-accent checked:before:translate-x-5 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:opacity-50"
      />
    </div>
  )
}

export function NumberInput({
  label,
  hint,
  value,
  onChange,
  min,
  max,
  suffix,
  disabled,
}: {
  label: string
  hint?: string
  value: number
  onChange: (value: number) => void
  min?: number
  max?: number
  suffix?: string
  disabled?: boolean
}) {
  const id = useId()
  return (
    <Label id={id} label={label} hint={hint}>
      <div className="flex items-center gap-2">
        <input
          id={id}
          type="number"
          inputMode="numeric"
          value={Number.isFinite(value) ? value : ''}
          min={min}
          max={max}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value === '' ? 0 : Number(event.target.value))}
          className={`${inputClass} max-w-40 font-mono`}
        />
        {suffix && <span className="text-sm text-muted">{suffix}</span>}
      </div>
    </Label>
  )
}

export function RangeInput({
  label,
  hint,
  value,
  onChange,
  min,
  max,
  format,
}: {
  label: string
  hint?: string
  value: number
  onChange: (value: number) => void
  min: number
  max: number
  format: (value: number) => string
}) {
  const id = useId()
  return (
    <Label id={id} label={label} hint={hint}>
      <div className="flex items-center gap-3">
        <input
          id={id}
          type="range"
          value={value}
          min={min}
          max={max}
          onChange={(event) => onChange(Number(event.target.value))}
          className="grow accent-(--color-accent)"
        />
        <output htmlFor={id} className="w-20 text-right font-mono text-sm">
          {format(value)}
        </output>
      </div>
    </Label>
  )
}

export function TextInput({
  label,
  hint,
  value,
  onChange,
  placeholder,
  type = 'text',
  disabled,
}: {
  label: string
  hint?: string
  value: string
  onChange: (value: string) => void
  placeholder?: string
  type?: 'text' | 'url' | 'password'
  disabled?: boolean
}) {
  const id = useId()
  return (
    <Label id={id} label={label} hint={hint}>
      <input
        id={id}
        type={type}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        autoComplete="off"
        spellCheck={false}
        onChange={(event) => onChange(event.target.value)}
        className={inputClass}
      />
    </Label>
  )
}

export function TextArea({
  label,
  hint,
  value,
  onChange,
  rows = 5,
  placeholder,
  disabled,
}: {
  label: string
  hint?: string
  value: string
  onChange: (value: string) => void
  rows?: number
  placeholder?: string
  disabled?: boolean
}) {
  const id = useId()
  return (
    <Label id={id} label={label} hint={hint}>
      <textarea
        id={id}
        value={value}
        rows={rows}
        placeholder={placeholder}
        disabled={disabled}
        spellCheck={false}
        onChange={(event) => onChange(event.target.value)}
        className={`${inputClass} h-auto py-2 font-mono text-[13px]`}
      />
    </Label>
  )
}

export function SelectInput<T extends string | number>({
  label,
  hint,
  value,
  options,
  onChange,
  disabled,
}: {
  label: string
  hint?: string
  value: T
  options: { value: T; label: string }[]
  onChange: (value: T) => void
  disabled?: boolean
}) {
  const id = useId()
  return (
    <Label id={id} label={label} hint={hint}>
      <select
        id={id}
        value={String(value)}
        disabled={disabled}
        onChange={(event) => {
          const picked = options.find((option) => String(option.value) === event.target.value)
          if (picked) onChange(picked.value)
        }}
        className={inputClass}
      >
        {options.map((option) => (
          <option key={String(option.value)} value={String(option.value)}>
            {option.label}
          </option>
        ))}
      </select>
    </Label>
  )
}

/** MatriX settings Rustorr keeps but does not act on yet, folded away. */
export function NotApplied({ children }: { children: ReactNode }) {
  const { t } = useTranslation()
  return (
    <details className="group rounded-xl border border-dashed border-line">
      <summary className="cursor-pointer list-none px-4 py-3 text-sm font-semibold text-muted [&::-webkit-details-marker]:hidden">
        <span className="mr-2 inline-block transition group-open:rotate-90">›</span>
        {t('settings.notApplied.title')}
      </summary>
      <div className="flex flex-col gap-5 border-t border-dashed border-line p-4">
        <p className="text-[13px] text-muted">{t('settings.notApplied.hint')}</p>
        {children}
      </div>
    </details>
  )
}
