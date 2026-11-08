import { devicesQuery } from '@holtmail/api-client'
import { Button } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useMemo, useState } from 'react'

import {
  deviceNoticesKey,
  formatNoticeDate,
  parseDismissedNotices,
  recentDeviceNotices,
} from '../lib/devices.ts'
import { strings } from '../strings.ts'

const t = strings.devices.notice

function readDismissed(): string[] {
  try {
    return parseDismissedNotices(window.localStorage.getItem(deviceNoticesKey))
  } catch {
    return []
  }
}

function writeDismissed(keys: string[]) {
  try {
    window.localStorage.setItem(deviceNoticesKey, JSON.stringify(keys))
  } catch {
    return
  }
}

export function DeviceNotices() {
  const devices = useQuery({ ...devicesQuery, refetchInterval: 5 * 60_000 }).data
  const [dismissed, setDismissed] = useState(readDismissed)
  const recent = useMemo(() => recentDeviceNotices(devices, Date.now()), [devices])
  const shown = recent.filter((notice) => !dismissed.includes(notice.key))
  const first = shown[0]
  if (!first) return null
  const date = formatNoticeDate(first.at)
  const lead =
    shown.length > 1
      ? t.many(shown.length)
      : first.kind === 'made'
        ? t.made(first.name, date)
        : t.raised(first.name, date)
  const dismiss = () => {
    const keys = recent.map((notice) => notice.key)
    writeDismissed(keys)
    setDismissed(keys)
  }
  return (
    <div
      role="status"
      className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-border px-4 py-2 text-sm"
    >
      <p className="min-w-0 flex-1 text-fg-muted">
        {lead} {t.notYou}
      </p>
      <Button size="sm" variant="ghost" asChild>
        <Link to="/settings/devices">{t.review}</Link>
      </Button>
      <Button size="sm" variant="ghost" onClick={dismiss}>
        {t.dismiss}
      </Button>
    </div>
  )
}
