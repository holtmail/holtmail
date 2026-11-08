import type { LocalEdit } from '@holtmail/api-client'
import { Badge } from '@holtmail/ui'

import { markState, markText } from '../lib/edits.ts'
import { strings } from '../strings.ts'

const t = strings.edits

const variants = {
  held: 'outline',
  sending: 'outline',
  conflict: 'warning',
  rejected: 'danger',
} as const

/** The hairline square "Local edit" mark of an item served from an open change. */
export function LocalEditMark({
  edit,
}: {
  edit: Pick<LocalEdit, 'state' | 'account_name' | 'reason'>
}) {
  const state = markState(edit)
  const text = markText(edit, t.mark)
  if (!state || !text) return null
  const reason = edit.reason ? t.reasons[edit.reason] : undefined
  return (
    <Badge variant={variants[state]} title={reason ?? t.mark.label} className="gap-1.5">
      <span aria-hidden className="size-1.5 border border-current" />
      <span className="sr-only">{t.mark.label}: </span>
      {text}
    </Badge>
  )
}
