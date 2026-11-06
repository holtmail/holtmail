import { cn } from '@holtmail/ui'
import type { KeyboardEvent, ReactNode } from 'react'

export function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-2">
      <dt className="w-24 shrink-0 text-fg-subtle">{label}</dt>
      <dd className="min-w-0 text-fg-muted wrap-break-word">{children}</dd>
    </div>
  )
}

export const insightRowSelector = '[data-insight-row]'

export function InsightRow({
  label,
  name,
  onKeyDown,
  children,
}: {
  label: string
  name: string
  onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => void
  children: ReactNode
}) {
  return (
    <div
      data-insight-row=""
      role="group"
      tabIndex={0}
      aria-label={name}
      onKeyDown={onKeyDown}
      className={cn(
        'flex items-center gap-2 py-0.5',
        'outline-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus',
      )}
    >
      <dt className="w-24 shrink-0 text-fg-subtle">{label}</dt>
      <dd className="flex min-w-0 flex-1 flex-wrap items-center gap-2 text-fg-muted">{children}</dd>
    </div>
  )
}
