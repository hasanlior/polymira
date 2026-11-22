/** Original line icons (24×24, stroke-based). No third-party logos. */
const P: Record<string, JSX.Element> = {
  home: <path d="M3 11.5 12 4l9 7.5M5.5 9.5V20h13V9.5M10 20v-5.5h4V20" />,
  disc: (
    <>
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="2.5" />
      <path d="M12 3a9 9 0 0 1 9 9M12 6.5a5.5 5.5 0 0 1 5.5 5.5" opacity=".6" />
    </>
  ),
  sphere: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M7 7.5c3 1.5 7 1.5 10 0M7 16.5c3-1.5 7-1.5 10 0M12 3c-2.5 3-2.5 15 0 18" />
    </>
  ),
  cartridge: (
    <>
      <rect x="5" y="3" width="14" height="18" rx="2" />
      <path d="M8 3v5h8V3M8 21v-3M11 21v-3M14 21v-3M17 21v-3" />
      <rect x="8" y="10.5" width="8" height="4" rx="1" />
    </>
  ),
  ring: (
    <>
      <path d="M12 3a9 9 0 1 0 9 9" />
      <path d="M12 7.5a4.5 4.5 0 1 0 4.5 4.5" />
      <path d="M21 3v6h-6" />
    </>
  ),
  joystick: (
    <>
      <rect x="4" y="15" width="16" height="5" rx="1.5" />
      <path d="M12 15V9" />
      <circle cx="12" cy="6.5" r="2.8" />
      <circle cx="17" cy="17.5" r=".6" fill="currentColor" />
    </>
  ),
  monitor: (
    <>
      <rect x="3" y="4" width="18" height="12" rx="1.5" />
      <path d="M9 20h6M12 16v4" />
    </>
  ),
  photo: (
    <>
      <rect x="3" y="5" width="18" height="14" rx="1.5" />
      <circle cx="9" cy="10" r="1.8" />
      <path d="m3 17 5-5 4 4 3-3 6 6" />
    </>
  ),
  gear: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2.5v3M12 18.5v3M4.2 6.5l2.2 1.6M17.6 15.9l2.2 1.6M4.2 17.5l2.2-1.6M17.6 8.1l2.2-1.6M2.5 12h3M18.5 12h3" />
      <circle cx="12" cy="12" r="6.5" />
    </>
  ),
  folder: <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5Z" />,
  plus: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7.5v9M7.5 12h9" />
    </>
  ),
  game: (
    <>
      <path d="M7 8h10a4 4 0 0 1 4 4l-.6 4.2a2.5 2.5 0 0 1-4.4 1.2L14.5 16h-5L8 17.4a2.5 2.5 0 0 1-4.4-1.2L3 12a4 4 0 0 1 4-4Z" />
      <path d="M8 10.5v3M6.5 12h3" />
      <circle cx="15.5" cy="11" r=".7" fill="currentColor" />
      <circle cx="17" cy="13" r=".7" fill="currentColor" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v6M12 7.5v.5" />
    </>
  ),
  display: (
    <>
      <rect x="3" y="4.5" width="18" height="12" rx="1.5" />
      <path d="M8 20h8" />
      <path d="M7 13l3-3 2 2 3-3 2 2" opacity=".7" />
    </>
  ),
  sound: (
    <>
      <path d="M4 9.5h3.5L12 6v12l-4.5-3.5H4Z" />
      <path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11" />
    </>
  ),
  chip: (
    <>
      <rect x="6" y="6" width="12" height="12" rx="1.5" />
      <rect x="9.5" y="9.5" width="5" height="5" />
      <path d="M9 3v3M12 3v3M15 3v3M9 18v3M12 18v3M15 18v3M3 9h3M3 12h3M3 15h3M18 9h3M18 12h3M18 15h3" />
    </>
  ),
  storage: (
    <>
      <rect x="3.5" y="5" width="17" height="6" rx="1.5" />
      <rect x="3.5" y="13" width="17" height="6" rx="1.5" />
      <path d="M7 8h.01M7 16h.01" strokeWidth="2.5" />
    </>
  ),
  palette: (
    <>
      <path d="M12 3a9 9 0 1 0 0 18c1.2 0 1.8-.9 1.4-1.9-.5-1.3.3-2.6 1.7-2.6H17a4 4 0 0 0 4-4c0-5-4-9.5-9-9.5Z" />
      <circle cx="7.5" cy="11" r="1" fill="currentColor" />
      <circle cx="10" cy="7" r="1" fill="currentColor" />
      <circle cx="14.5" cy="7" r="1" fill="currentColor" />
    </>
  ),
  refresh: (
    <>
      <path d="M20 11a8 8 0 0 0-14.5-4.5L4 8.5M4 13a8 8 0 0 0 14.5 4.5l1.5-2" />
      <path d="M4 4v4.5h4.5M20 20v-4.5h-4.5" />
    </>
  ),
  trash: <path d="M4.5 6.5h15M9.5 6.5V4h5v2.5M6.5 6.5l1 13.5h9l1-13.5M10 10v6.5M14 10v6.5" />,
  star: <path d="m12 3.5 2.6 5.4 5.9.8-4.3 4.1 1 5.8L12 16.8l-5.2 2.8 1-5.8-4.3-4.1 5.9-.8Z" />,
  check: <path d="m5 12.5 4.5 4.5L19 7.5" />,
  power: (
    <>
      <path d="M12 3v8" />
      <path d="M7 6.2a7.5 7.5 0 1 0 10 0" />
    </>
  ),
  pad: (
    <>
      <path d="M7 8h10a4 4 0 0 1 4 4l-.6 4.2a2.5 2.5 0 0 1-4.4 1.2L14.5 16h-5L8 17.4a2.5 2.5 0 0 1-4.4-1.2L3 12a4 4 0 0 1 4-4Z" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </>
  ),
  play: <path d="M8 5.5v13l10.5-6.5Z" />,
  bag: (
    <>
      <path d="M5 8h14l-1 12H6Z" />
      <path d="M9 8V6.5a3 3 0 0 1 6 0V8" />
    </>
  ),
  apps: (
    <>
      <rect x="3.5" y="5" width="17" height="12" rx="2" />
      <path d="M10 9v4.5l4-2.25Z" />
      <path d="M8 20h8" />
    </>
  ),
  brush: (
    <>
      <path d="M14.5 4.5 19.5 9.5 11 18l-5-5Z" />
      <path d="M6 13c-2 0-3 1.5-3 3.5S2.5 20 2.5 20s3.5.5 5.5-1 2-3 2-3" />
    </>
  ),
  pause: <path d="M8 5.5v13M16 5.5v13" />,
  camera: (
    <>
      <path d="M4 8.5A1.5 1.5 0 0 1 5.5 7h2.3l1.5-2h5.4l1.5 2h2.3A1.5 1.5 0 0 1 20 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 4 17.5Z" />
      <circle cx="12" cy="13" r="3.5" />
    </>
  ),
  exit: (
    <>
      <path d="M14 4.5H6.5A1.5 1.5 0 0 0 5 6v12a1.5 1.5 0 0 0 1.5 1.5H14" />
      <path d="M10 12h10M16.5 8.5 20 12l-3.5 3.5" />
    </>
  ),
  grid: (
    <>
      <rect x="4" y="4" width="6.5" height="6.5" rx="1" />
      <rect x="13.5" y="4" width="6.5" height="6.5" rx="1" />
      <rect x="4" y="13.5" width="6.5" height="6.5" rx="1" />
      <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m16 16 4.5 4.5" />
    </>
  ),
  chart: <path d="M4 20V10M10 20V4M16 20v-7M21 20H3" />,
  user: (
    <>
      <circle cx="12" cy="8.5" r="4" />
      <path d="M4.5 20a7.5 7.5 0 0 1 15 0" />
    </>
  ),
  file: (
    <>
      <path d="M6 3h8l4 4v14H6Z" />
      <path d="M14 3v4h4" />
    </>
  ),
  drive: (
    <>
      <rect x="3" y="7" width="18" height="10" rx="2" />
      <path d="M7 12h.01M17 12h4" strokeWidth="2" />
    </>
  ),
  up: <path d="M12 19V5M6 11l6-6 6 6" />,
  warning: (
    <>
      <path d="M12 3.5 21.5 20h-19Z" />
      <path d="M12 10v4.5M12 17v.5" />
    </>
  )
}

export type IconName = keyof typeof P

export function Icon({ name, className }: { name: IconName | string; className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.4"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {P[name] ?? P.game}
    </svg>
  )
}

/** Platform badge: rounded tile with the platform's short name. */
export function Badge({ text, hue }: { text: string; hue: number }) {
  return (
    <div className="badge" style={{ ['--h' as string]: hue }}>
      <span>{text}</span>
    </div>
  )
}
