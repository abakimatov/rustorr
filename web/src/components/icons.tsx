import type { SVGProps } from 'react'

type IconProps = SVGProps<SVGSVGElement> & { size?: number }

function stroke({ size = 20, children, ...props }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  )
}

export const LogoIcon = (props: IconProps) =>
  stroke({ ...props, strokeWidth: 2, children: <><circle cx="12" cy="12" r="9" /><path d="M10 8.5v7l5.5-3.5z" fill="currentColor" /></> })
export const ListIcon = (props: IconProps) => stroke({ ...props, children: <path d="M4 6h16M4 12h16M4 18h10" /> })
export const SearchIcon = (props: IconProps) =>
  stroke({ ...props, children: <><circle cx="11" cy="11" r="6.5" /><path d="M16 16l4 4" /></> })
export const SettingsIcon = (props: IconProps) =>
  stroke({
    ...props,
    children: <><circle cx="12" cy="12" r="3" /><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1" /></>,
  })
export const PlusIcon = (props: IconProps) => stroke({ ...props, strokeWidth: 2.2, children: <path d="M12 5v14M5 12h14" /> })
export const PlayIcon = ({ size = 18, ...props }: IconProps) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" {...props}>
    <path d="M8 5.5v13l10.5-6.5z" />
  </svg>
)
export const PlaylistIcon = (props: IconProps) => stroke({ ...props, children: <path d="M4 6h11M4 12h11M4 18h7M17 14v6l4-3z" /> })
export const MoreIcon = ({ size = 18, ...props }: IconProps) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" {...props}>
    <circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" />
  </svg>
)
export const BackIcon = (props: IconProps) => stroke({ ...props, children: <path d="M15 6l-6 6 6 6" /> })
export const CloseIcon = (props: IconProps) => stroke({ ...props, strokeWidth: 2, children: <path d="M6 6l12 12M18 6L6 18" /> })
export const CopyIcon = (props: IconProps) =>
  stroke({ ...props, children: <><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a1 1 0 0 1 1-1h9" /></> })
export const LinkIcon = (props: IconProps) =>
  stroke({ ...props, children: <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" /> })
export const SunIcon = (props: IconProps) =>
  stroke({ ...props, children: <><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></> })
export const MoonIcon = (props: IconProps) => stroke({ ...props, children: <path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z" /> })
export const MonitorIcon = (props: IconProps) =>
  stroke({ ...props, children: <><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" /></> })
export const UploadIcon = (props: IconProps) => stroke({ ...props, children: <path d="M12 16V4M7 9l5-5 5 5M4 16v3a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3" /> })
export const CheckIcon = (props: IconProps) => stroke({ ...props, strokeWidth: 2.2, children: <path d="M5 12.5l4.5 4.5L19 7.5" /> })
export const EjectIcon = (props: IconProps) => stroke({ ...props, children: <path d="M5 16h14M12 5l7 8H5z" /> })
export const TrashIcon = (props: IconProps) => stroke({ ...props, children: <path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13" /> })
