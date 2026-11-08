import {
  ApiError,
  cancelOutbox,
  type Rsvp,
  type RsvpAnswer,
  type RsvpOptions,
  rsvpItem,
  rsvpItemOptionsQuery,
  rsvpMessage,
  rsvpMessageOptionsQuery,
  resolvePimChange,
} from '@holtmail/api-client'
import { Button, ToggleGroup, ToggleGroupItem } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useEffect, useMemo, useState } from 'react'

import { FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatWhen, viewerZone } from '../lib/events.ts'
import { canUndo, undoSeconds } from '../lib/outbox.ts'
import { defaultOccurrence, doneNote, isHeld, listsOccurrence, routeNote } from '../lib/rsvp.ts'
import { strings } from '../strings.ts'
import { SmallInput, NativeSelect } from './inputs.tsx'
import { LocalEditMark } from './local-edit-mark.tsx'

const t = strings.edits.rsvp

const ANSWERS: readonly RsvpAnswer[] = ['accepted', 'tentative', 'declined']

export type RsvpTarget = { kind: 'message' | 'item'; id: string }

const optionsQuery = (target: RsvpTarget) =>
  target.kind === 'message' ? rsvpMessageOptionsQuery(target.id) : rsvpItemOptionsQuery(target.id)

/** Accept, Maybe and Decline for an invitation, with how the answer would leave. Renders
 * nothing unless the server says the user may answer it now. */
export function RsvpPanel({
  target,
  archiveId,
  occurrence,
}: {
  target: RsvpTarget
  archiveId: string
  occurrence?: string
}) {
  const options = useQuery(optionsQuery(target))
  if (options.data?.reason === 'open_edit') {
    return <p className="border-t border-border pt-3 text-xs text-fg-muted">{t.openEdit}</p>
  }
  if (!options.data?.allowed) return null
  return (
    <RsvpForm
      target={target}
      archiveId={archiveId}
      options={options.data}
      occurrence={occurrence}
    />
  )
}

function RsvpForm({
  target,
  archiveId,
  options,
  occurrence,
}: {
  target: RsvpTarget
  archiveId: string
  options: RsvpOptions
  occurrence?: string
}) {
  const queryClient = useQueryClient()
  const zone = useMemo(() => viewerZone(), [])
  const [note, setNote] = useState<string>()
  const [scope, setScope] = useState<'series' | 'one'>(
    listsOccurrence(options.occurrences, occurrence) ? 'one' : 'series',
  )
  const first = defaultOccurrence(options.occurrences, occurrence, Date.now())
  const [picked, setPicked] = useState(first?.recurrence_id ?? '')
  const [result, setResult] = useState<{ answer: RsvpAnswer; rsvp: Rsvp }>()
  const [undone, setUndone] = useState(false)
  const [kept, setKept] = useState(false)
  const [now, setNow] = useState(() => Date.now())

  const outbox = result?.rsvp.outbox ?? undefined
  useEffect(() => {
    if (!outbox) return undefined
    const timer = window.setInterval(() => setNow(Date.now()), 250)
    return () => window.clearInterval(timer)
  }, [outbox])

  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['items'] }),
      queryClient.invalidateQueries({ queryKey: ['messages'] }),
      queryClient.invalidateQueries({ queryKey: ['threads'] }),
      queryClient.invalidateQueries({ queryKey: ['events'] }),
      queryClient.invalidateQueries({ queryKey: ['pim-changes'] }),
      queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'calendar'] }),
    ])

  const answer = useMutation({
    mutationFn: async (given: RsvpAnswer) => {
      const request = {
        answer: given,
        comment: note?.trim() || null,
        recurrence_id: options.recurring && scope === 'one' && picked ? picked : null,
      }
      const rsvp =
        target.kind === 'message'
          ? await rsvpMessage(target.id, request)
          : await rsvpItem(target.id, request)
      return { answer: given, rsvp }
    },
    onSuccess: async (done) => {
      setResult(done)
      setUndone(false)
      setKept(false)
      setNote(undefined)
      setNow(Date.now())
      await refresh()
    },
  })

  const undo = useMutation({
    mutationFn: async () => {
      if (!result?.rsvp.outbox) throw new Error(t.tooLate)
      try {
        await cancelOutbox(result.rsvp.outbox.id)
      } catch (error) {
        if (error instanceof ApiError && error.status === 409)
          throw new Error(t.tooLate, { cause: error })
        throw error
      }
      if (result.rsvp.change) {
        await resolvePimChange(result.rsvp.change.id, 'discard').catch(() => {
          setKept(true)
        })
      }
    },
    onSuccess: async () => {
      setResult(undefined)
      setUndone(true)
      await refresh()
    },
  })

  const current = result?.answer ?? options.my_status
  const waiting = outbox && canUndo(outbox, now)
  const route = routeNote(options)
  const account = options.account_name ?? strings.edits.mark.account
  const choices = options.occurrences.map((o) => ({
    value: o.recurrence_id,
    label:
      formatWhen(
        { value: o.start_local, time_zone: o.time_zone, utc: o.starts_at },
        undefined,
        o.all_day,
        zone,
      )?.local ?? o.start_local,
  }))

  return (
    <section aria-label={t.label} className="grid gap-3 border-t border-border pt-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        {ANSWERS.map((value) => (
          <Button
            key={value}
            size="sm"
            variant={current === value ? 'primary' : 'secondary'}
            aria-pressed={current === value}
            disabled={answer.isPending || undo.isPending || Boolean(waiting)}
            onClick={() => answer.mutate(value)}
          >
            {t.answers[value]}
          </Button>
        ))}
        {note === undefined && (
          <Button variant="ghost" size="sm" onClick={() => setNote('')}>
            {t.addNote}
          </Button>
        )}
        {options.local_edit && <LocalEditMark edit={options.local_edit} />}
      </div>
      {options.recurring && choices.length > 0 && (
        <div className="flex flex-wrap items-end gap-3">
          <ToggleGroup
            type="single"
            size="sm"
            aria-label={t.scope}
            value={scope}
            onValueChange={(value) => {
              if (value === 'series' || value === 'one') setScope(value)
            }}
          >
            <ToggleGroupItem value="series">{t.series}</ToggleGroupItem>
            <ToggleGroupItem value="one">{t.one}</ToggleGroupItem>
          </ToggleGroup>
          {scope === 'one' && (
            <NativeSelect
              label={t.occurrence}
              hideLabel
              value={picked}
              onChange={setPicked}
              choices={choices}
            />
          )}
        </div>
      )}
      {note !== undefined && <SmallInput label={t.note} value={note} onChange={setNote} />}
      {route && <p className="text-xs text-fg-subtle">{route}</p>}
      {isHeld(options) && <p className="text-xs text-fg-muted">{t.held(account)}</p>}
      <div role="status" aria-live="polite" className="contents">
        {result && (
          <p className="sr-only">{waiting ? t.done.pending : doneNote(result.rsvp, options)}</p>
        )}
        {undone && (
          <p className="text-xs text-fg-muted">{kept ? t.done.undoneMailOnly : t.done.undone}</p>
        )}
      </div>
      {result && (
        <p className="flex flex-wrap items-center gap-3 text-xs text-fg-muted">
          <span aria-hidden="true">
            {waiting && outbox
              ? t.done.waiting(undoSeconds(outbox, now))
              : doneNote(result.rsvp, options)}
          </span>
          {waiting && (
            <Button
              variant="secondary"
              size="sm"
              disabled={undo.isPending}
              onClick={() => undo.mutate()}
            >
              {undo.isPending ? t.undoing : t.undo}
            </Button>
          )}
          {outbox && (
            <Link
              to="/a/$archiveId/outbox"
              params={{ archiveId }}
              className="text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            >
              {t.openOutbox}
            </Link>
          )}
        </p>
      )}
      <FormError
        message={
          answer.isError
            ? `${t.failed}: ${authErrorMessage(answer.error)}`
            : undo.isError
              ? authErrorMessage(undo.error)
              : undefined
        }
      />
    </section>
  )
}
