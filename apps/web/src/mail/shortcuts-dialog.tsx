import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Kbd,
} from '@holtmail/ui'
import { Fragment } from 'react'

import { isMac, shortcutList } from '../lib/keymap.ts'
import { displayStroke } from '../lib/shortcuts.ts'
import { strings } from '../strings.ts'

export function ShortcutsDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{strings.shortcuts.title}</DialogTitle>
          <DialogDescription>{strings.shortcuts.description}</DialogDescription>
        </DialogHeader>
        <dl className="grid">
          {shortcutList.map(({ action, keys }) => (
            <div
              key={action}
              className="flex h-10 items-center justify-between gap-4 border-b border-border text-sm last:border-b-0"
            >
              <dt className="text-fg-muted">{strings.shortcuts.items[action]}</dt>
              <dd className="flex items-center gap-1 text-xs text-fg-subtle">
                {keys.map((key, index) => (
                  <Fragment key={key}>
                    {index > 0 && <span className="px-1">{strings.shortcuts.or}</span>}
                    {displayStroke(key, isMac).map((stroke, i) => (
                      <Fragment key={i}>
                        {i > 0 && <span className="px-0.5">{strings.shortcuts.sequence}</span>}
                        <Kbd>{stroke}</Kbd>
                      </Fragment>
                    ))}
                  </Fragment>
                ))}
              </dd>
            </div>
          ))}
        </dl>
      </DialogContent>
    </Dialog>
  )
}
