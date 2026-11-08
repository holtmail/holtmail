import { type Draft, type Outbox, undoOutbox } from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Button } from '@holtmail/ui'
import { useMutation } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { XIcon } from 'lucide-react'
import { m } from 'motion/react'
import { useEffect, useRef, useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { canUndo, isScheduled, undoSeconds } from '../lib/outbox.ts'
import { strings } from '../strings.ts'

const t = strings.writing.undo

const AFTER_SEND_MS = 6_000

/** The bar after a send: counts down the undo window and calls undo inside it; a scheduled
 * message can be undone until it leaves. */
export function UndoToast({
  archiveId,
  entry,
  problem,
  onUndone,
  onDismiss,
}: {
  archiveId: string
  entry: Outbox | undefined
  problem: string | undefined
  onUndone: (draft: Draft) => void
  onDismiss: () => void
}) {
  const [now, setNow] = useState(() => Date.now())
  const scheduled = entry ? isScheduled(entry) : false
  const open = entry ? canUndo(entry, now) : false
  const dismiss = useRef(onDismiss)
  dismiss.current = onDismiss

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 250)
    return () => window.clearInterval(timer)
  }, [])

  useEffect(() => {
    if (!entry || scheduled) return undefined
    const left = entry.send_at - Date.now() + AFTER_SEND_MS
    const timer = window.setTimeout(() => dismiss.current(), Math.max(AFTER_SEND_MS, left))
    return () => window.clearTimeout(timer)
  }, [entry, scheduled])

  const undo = useMutation({
    mutationFn: () => {
      if (!entry) throw new Error('nothing to undo')
      return undoOutbox(entry.id)
    },
    onSuccess: (result) => onUndone(result.draft),
  })

  const message = problem
    ? problem
    : undo.isError
      ? `${t.tooLate} ${authErrorMessage(undo.error)}`
      : !entry
        ? ''
        : scheduled
          ? t.scheduled(formatFullDate(entry.send_at))
          : open
            ? `${t.queued}. ${t.sendingIn(undoSeconds(entry, now))}`
            : t.queued

  return (
    <m.div
      role="status"
      aria-live="polite"
      initial={{ opacity: 0, y: distance.sm }}
      animate={{ opacity: 1, y: 0, transition: transition.productive.moderate01 }}
      className="fixed inset-x-4 bottom-4 z-50 mx-auto flex max-w-lg items-center gap-3 border border-border-strong bg-bg px-4 py-2 text-sm text-fg"
    >
      <span className="min-w-0 flex-1">{message}</span>
      {entry && open && !undo.isError && (
        <Button
          variant="secondary"
          size="sm"
          disabled={undo.isPending}
          onClick={() => undo.mutate()}
        >
          {undo.isPending ? t.undoing : t.undo}
        </Button>
      )}
      {entry && (
        <Button variant="ghost" size="sm" asChild>
          <Link to="/a/$archiveId/outbox" params={{ archiveId }}>
            {t.openOutbox}
          </Link>
        </Button>
      )}
      <Button variant="ghost" size="icon" aria-label={t.dismiss} onClick={onDismiss}>
        <XIcon />
      </Button>
    </m.div>
  )
}
