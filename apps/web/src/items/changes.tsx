import type { ItemChange } from '@holtmail/api-client'
import { cn } from '@holtmail/ui'

import { changeKind, fieldLabel } from '../lib/events.ts'
import { strings } from '../strings.ts'

export function Changes({
  number,
  readable,
  changes,
}: {
  number: number
  readable: boolean
  changes: ItemChange[]
}) {
  if (!readable) return <p className="text-sm text-fg-subtle">{strings.item.unreadable}</p>
  if (changes.length === 0) {
    return (
      <p className="text-sm text-fg-subtle">
        {number === 1 ? strings.item.first : strings.item.noChanges}
      </p>
    )
  }
  return (
    <ul className="grid gap-0.5 text-sm">
      {changes.map((change, index) => {
        const kind = changeKind(change)
        return (
          <li key={`${change.field}-${index}`} className="flex flex-wrap gap-x-2">
            <span className="w-32 shrink-0 text-fg-subtle">{fieldLabel(change.field)}</span>
            <span className="min-w-0 flex-1 wrap-break-word">
              {kind !== 'added' && (
                <span className={cn('text-fg-muted', kind === 'removed' && 'line-through')}>
                  {change.before}
                </span>
              )}
              {kind === 'changed' && <span className="px-1 text-fg-subtle">→</span>}
              {kind !== 'removed' && <span className="text-fg">{change.after}</span>}
            </span>
          </li>
        )
      })}
    </ul>
  )
}
