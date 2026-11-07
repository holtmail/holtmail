import type { InsightsStatus } from '@holtmail/api-client'

export const READING_POLL_MS = 5000

/** Messages still waiting for insights, categories or a search update, or `null` when there is nothing to show. */
export function stillReading(status: InsightsStatus | undefined): number | null {
  if (!status) return null
  const left = status.pending + status.categorize + status.reindex
  return left > 0 ? left : null
}
