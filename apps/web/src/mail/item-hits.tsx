import { type ItemHit, itemSearchQuery } from '@holtmail/api-client'
import { cn } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { CalendarIcon, ContactIcon } from 'lucide-react'
import type { ReactNode } from 'react'

import { strings } from '../strings.ts'

const shown = 5

const rowClass =
  'flex items-baseline gap-2 px-4 py-1.5 text-sm outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus'

function when(hit: ItemHit): string | undefined {
  if (hit.starts_at == null) return undefined
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    ...(hit.all_day ? { timeZone: 'UTC' } : {}),
  }).format(new Date(hit.starts_at))
}

function HitLink({
  archiveId,
  hit,
  children,
}: {
  archiveId: string
  hit: ItemHit
  children: ReactNode
}) {
  if (hit.kind === 'event' && hit.group_id) {
    return (
      <Link to="/events/$groupId" params={{ groupId: hit.group_id }} className={rowClass}>
        {children}
      </Link>
    )
  }
  if (hit.kind === 'contact' && hit.person_id) {
    return (
      <Link
        to="/a/$archiveId/people"
        params={{ archiveId }}
        search={{ p: hit.person_id }}
        className={rowClass}
      >
        {children}
      </Link>
    )
  }
  return (
    <Link to="/items/$itemId" params={{ itemId: hit.item_id }} className={rowClass}>
      {children}
    </Link>
  )
}

export function ItemHits({ archiveId, q }: { archiveId: string; q: string }) {
  const hits = useQuery({ ...itemSearchQuery(archiveId, q), enabled: q !== '' })
  const items = hits.data?.items ?? []
  if (items.length === 0) return null
  const rest = (hits.data?.total ?? items.length) - Math.min(items.length, shown)
  return (
    <section aria-label={strings.search.items} className="shrink-0 border-b border-border py-1">
      <h2 className="flex h-6 items-center gap-2 px-4 text-xs text-fg-subtle">
        {strings.search.items}
        {rest > 0 && <span className="font-mono">{strings.search.more(rest)}</span>}
      </h2>
      <ul>
        {items.slice(0, shown).map((hit) => {
          const Icon = hit.kind === 'event' ? CalendarIcon : ContactIcon
          const date = when(hit)
          return (
            <li key={hit.item_id}>
              <HitLink archiveId={archiveId} hit={hit}>
                <Icon
                  aria-label={hit.kind === 'event' ? strings.search.event : strings.search.contact}
                  className="size-3 shrink-0 self-center text-fg-subtle"
                />
                <span
                  className={cn(
                    'min-w-0 flex-1 truncate text-fg',
                    hit.removed_at != null && 'text-fg-muted line-through',
                  )}
                >
                  {hit.title || strings.calendar.untitled}
                </span>
                {date && (
                  <span className="shrink-0 font-mono text-xs text-fg-subtle tabular-nums">
                    {date}
                  </span>
                )}
              </HitLink>
            </li>
          )
        })}
      </ul>
    </section>
  )
}
