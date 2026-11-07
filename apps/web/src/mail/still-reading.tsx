import { insightsStatusQuery } from '@holtmail/api-client'
import { cn } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'

import { formatCount } from '../lib/format.ts'
import { READING_POLL_MS, stillReading } from '../lib/reading.ts'
import { strings } from '../strings.ts'

export function StillReading({ archiveId, className }: { archiveId: string; className?: string }) {
  const status = useQuery({
    ...insightsStatusQuery(archiveId),
    retry: false,
    refetchInterval: (query) => (stillReading(query.state.data) === null ? false : READING_POLL_MS),
  })
  const left = stillReading(status.data)
  return (
    <p
      role="status"
      aria-live="polite"
      className={cn('text-xs text-fg-subtle empty:hidden', className)}
    >
      {left === null ? null : strings.insights.stillReading(formatCount(left))}
    </p>
  )
}
