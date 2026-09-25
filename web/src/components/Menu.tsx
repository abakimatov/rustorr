import { type ReactNode, useEffect, useRef, useState } from 'react'

/** A small dropdown on `<details>`: keyboard and screen readers get a native
 * disclosure; picking an item, Escape or a click elsewhere closes it. */
export function Menu({ label, icon, children }: { label: string; icon: ReactNode; children: ReactNode }) {
  const ref = useRef<HTMLDetailsElement>(null)
  const [open, setOpen] = useState(false)
  useEffect(() => {
    if (!open) return
    const close = () => ref.current?.removeAttribute('open')
    const onPointer = (event: PointerEvent) => {
      if (!ref.current?.contains(event.target as Node)) close()
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        close()
        ref.current?.querySelector('summary')?.focus()
      }
    }
    document.addEventListener('pointerdown', onPointer)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('pointerdown', onPointer)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])
  return (
    <details ref={ref} className="relative" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary
        aria-label={label}
        title={label}
        className="flex size-10 cursor-pointer list-none items-center justify-center rounded-lg border border-line text-muted hover:bg-raised hover:text-ink [&::-webkit-details-marker]:hidden"
      >
        {icon}
      </summary>
      <div
        role="menu"
        onClick={() => ref.current?.removeAttribute('open')}
        className="absolute right-0 z-20 mt-2 flex min-w-56 flex-col rounded-xl border border-line bg-surface p-1.5 shadow-lg"
      >
        {children}
      </div>
    </details>
  )
}

export function MenuItem({
  children,
  onSelect,
  href,
  danger = false,
}: {
  children: ReactNode
  onSelect?: () => void
  href?: string
  danger?: boolean
}) {
  const className = `flex items-center gap-3 rounded-lg px-3 py-2.5 text-left text-[15px] no-underline hover:bg-raised ${
    danger ? 'text-danger' : 'text-ink'
  }`
  if (href) {
    return (
      <a role="menuitem" href={href} className={className}>
        {children}
      </a>
    )
  }
  return (
    <button type="button" role="menuitem" onClick={onSelect} className={className}>
      {children}
    </button>
  )
}
