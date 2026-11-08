import {
  cancelOutbox,
  type Draft,
  type MailChange,
  draftsQuery,
  idempotencyKey,
  type Outbox,
  outboxQuery,
  resendOutbox,
  retryOutbox,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useParams } from '@tanstack/react-router'
import { CircleAlertIcon, FilePenIcon, SendIcon } from 'lucide-react'
import { useState } from 'react'

import { useCompose } from '../compose/compose-host.tsx'
import { useNow } from '../hooks/use-now.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate, formatListDate } from '../lib/format.ts'
import {
  groupOutbox,
  isOutboxState,
  isScheduled,
  outboxActions,
  refusedRecipients,
  resendRecipients,
} from '../lib/outbox.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { EmptyState } from './empty-state.tsx'
import { errorText } from './mailbox-actions.tsx'

const t = strings.writing.outbox

const tone: Record<string, 'neutral' | 'accent' | 'outline'> = {
  failed: 'outline',
  uncertain: 'outline',
  bounced: 'outline',
}

/** The archive's outbox: drafts, then messages grouped by what became of them, with Retry,
 * Resend and Cancel. */
export function OutboxPage() {
  const { archiveId } = useParams({ from: '/a/$archiveId/outbox' })
  const outbox = useQuery(outboxQuery(archiveId))
  const drafts = useQuery(draftsQuery(archiveId))
  const groups = groupOutbox(outbox.data ?? [])

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto grid w-full max-w-3xl gap-8 px-6 py-8 max-sm:px-4">
        <div className="grid gap-1">
          <h2 className="text-2xl font-normal">{t.title}</h2>
          <p className="text-sm text-fg-muted">{t.lead}</p>
        </div>
        {drafts.data && drafts.data.length > 0 && <Drafts drafts={drafts.data} />}
        {outbox.isPending ? (
          <Skeleton className="h-48" />
        ) : outbox.isError ? (
          <p role="alert" className="text-sm text-danger">
            {t.loadFailed}: {authErrorMessage(outbox.error)}
          </p>
        ) : groups.length === 0 ? (
          <EmptyState icon={<SendIcon />} title={t.empty} />
        ) : (
          groups.map(({ group, entries }) => (
            <section key={group} className="grid gap-2" aria-labelledby={`outbox-${group}`}>
              <h3 id={`outbox-${group}`} className="text-xs text-fg-subtle">
                {t.groups[group]} · {entries.length}
              </h3>
              {group === 'uncertain' && (
                <div className="flex gap-3 border border-border bg-bg-subtle p-3 text-sm">
                  <CircleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-warning" />
                  <div className="grid gap-1">
                    <p className="text-fg">{t.uncertainTitle}</p>
                    <p className="text-fg-muted">{t.uncertainExplain}</p>
                  </div>
                </div>
              )}
              <ul className="grid divide-y divide-border border-y border-border">
                {entries.map((entry) => (
                  <OutboxRow key={entry.id} entry={entry} archiveId={archiveId} />
                ))}
              </ul>
            </section>
          ))
        )}
      </div>
    </div>
  )
}

function Drafts({ drafts }: { drafts: Draft[] }) {
  const compose = useCompose()
  const now = useNow()
  const w = strings.writing
  return (
    <section className="grid gap-2" aria-labelledby="outbox-drafts">
      <h3 id="outbox-drafts" className="text-xs text-fg-subtle">
        {strings.folders.roles.drafts} · {drafts.length}
      </h3>
      <ul className="grid divide-y divide-border border-y border-border">
        {drafts.map((draft) => {
          const to = [...(draft.payload.to ?? []), ...(draft.payload.cc ?? [])]
          return (
            <li key={draft.id} className="flex items-center gap-3 py-3">
              <FilePenIcon aria-hidden className="size-4 shrink-0 text-fg-subtle" />
              <div className="grid min-w-0 flex-1 gap-0.5">
                <span className="truncate text-sm">
                  {draft.payload.subject?.trim() || t.noSubject}
                </span>
                {to.length > 0 && (
                  <span className="truncate text-xs text-fg-subtle">
                    {t.to(to.map((m) => m.name || m.address).join(', '))}
                  </span>
                )}
              </div>
              <span className="font-mono text-xs text-fg-subtle tabular-nums">
                {formatListDate(draft.saved_at, now)}
              </span>
              {compose.available && (
                <Button variant="secondary" size="sm" onClick={() => compose.openDraft(draft.id)}>
                  {w.sending.edit}
                </Button>
              )}
            </li>
          )
        })}
      </ul>
    </section>
  )
}

function OutboxRow({ entry, archiveId }: { entry: Outbox; archiveId: string }) {
  const queryClient = useQueryClient()
  const [confirm, setConfirm] = useState<'retry' | 'resend' | 'cancel' | null>(null)
  const compose = useCompose()
  const actions = outboxActions(entry, { compose: compose.available })
  const refused = refusedRecipients(entry)
  const accepted = entry.recipients.filter((r) => r.state === 'accepted').length
  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'outbox'] })
  const act = useMutation({
    mutationFn: async (kind: 'retry' | 'resend' | 'resendRefused' | 'cancel') => {
      if (kind === 'cancel') return cancelOutbox(entry.id)
      if (kind === 'retry') return retryOutbox(entry.id, idempotencyKey())
      return resendOutbox(entry.id, idempotencyKey(), resendRecipients(entry, kind))
    },
    onSettled: refresh,
  })
  const state = entry.state
  const label = isScheduled(entry)
    ? t.scheduledState
    : isOutboxState(state)
      ? t.states[state]
      : state

  return (
    <li className="grid gap-2 py-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="min-w-0 flex-1 truncate text-sm text-fg">
          {entry.subject?.trim() || t.noSubject}
        </span>
        <Badge variant={tone[state] ?? 'neutral'}>{label}</Badge>
      </div>
      <div className="grid gap-0.5 text-xs text-fg-muted">
        <span className="truncate">{t.to(entry.recipients.map((r) => r.address).join(', '))}</span>
        {entry.route && <span className="truncate">{t.from(entry.route.from)}</span>}
        <span className="flex flex-wrap gap-x-3 text-fg-subtle">
          {entry.sent_at != null ? (
            <span>{t.sentAt(formatFullDate(entry.sent_at))}</span>
          ) : state === 'queued' ? (
            <span>{t.sendAt(formatFullDate(entry.send_at))}</span>
          ) : (
            <span>{t.created(formatFullDate(entry.created_at))}</span>
          )}
          {entry.attempts > 0 && <span>{t.attempts(entry.attempts)}</span>}
          {state === 'retrying' && entry.next_attempt_at != null && (
            <span>{t.nextAttempt(formatFullDate(entry.next_attempt_at))}</span>
          )}
          {accepted > 0 && refused.length > 0 && <span>{t.accepted(accepted)}</span>}
          {entry.resend_of && <span>{t.resendOf}</span>}
        </span>
        {entry.recipients
          .filter((r) => r.state === 'refused')
          .map((r) => (
            <span key={r.address} className="text-danger">
              {t.refused(r.address, [r.code, r.text].filter(Boolean).join(' '))}
            </span>
          ))}
        {entry.recipients
          .filter((r) => r.state === 'bounced')
          .map((r) => (
            <span key={r.address} className="text-danger">
              {t.bounced(r.address, [r.status, r.text].filter(Boolean).join(' '))}
            </span>
          ))}
        {entry.note && <span className="text-warning">{entry.note}</span>}
        {entry.sent_copy && <SentCopyLine change={entry.sent_copy} />}
        {entry.last_error && state !== 'sent' && (
          <span className="text-danger">{t.lastError(entry.last_error)}</span>
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {actions.retry && (
          <Button
            variant="secondary"
            size="sm"
            disabled={act.isPending}
            onClick={() => (actions.confirmRetry ? setConfirm('retry') : act.mutate('retry'))}
          >
            {t.retry}
          </Button>
        )}
        {actions.resendRefused && (
          <Button
            variant="secondary"
            size="sm"
            disabled={act.isPending}
            onClick={() => act.mutate('resendRefused')}
          >
            {t.resendRefused}
          </Button>
        )}
        {actions.resend && (
          <Button
            variant="ghost"
            size="sm"
            disabled={act.isPending}
            onClick={() => (actions.confirmResend ? setConfirm('resend') : act.mutate('resend'))}
          >
            {t.resend}
          </Button>
        )}
        {actions.cancel && (
          <Button
            variant="ghost"
            size="sm"
            disabled={act.isPending}
            onClick={() => setConfirm('cancel')}
          >
            {t.cancel}
          </Button>
        )}
        {entry.message && (
          <Button variant="ghost" size="sm" asChild>
            <Link to="/a/$archiveId" params={{ archiveId }} search={{ m: entry.message }}>
              {t.openSent}
            </Link>
          </Button>
        )}
      </div>
      {act.isError && (
        <p role="alert" className="text-xs text-danger">
          {authErrorMessage(act.error)}
        </p>
      )}
      <ConfirmDialog
        open={confirm === 'retry'}
        onOpenChange={(open) => setConfirm(open ? 'retry' : null)}
        title={t.confirmUncertainTitle}
        description={t.confirmUncertainLead}
        action={t.confirmSend}
        busyAction={t.working}
        onConfirm={() => act.mutateAsync('retry')}
      />
      <ConfirmDialog
        open={confirm === 'resend'}
        onOpenChange={(open) => setConfirm(open ? 'resend' : null)}
        title={t.confirmResendTitle}
        description={t.confirmResendLead}
        action={t.confirmSend}
        busyAction={t.working}
        onConfirm={() => act.mutateAsync('resend')}
      />
      <ConfirmDialog
        open={confirm === 'cancel'}
        onOpenChange={(open) => setConfirm(open ? 'cancel' : null)}
        title={t.confirmCancelTitle}
        description={t.confirmCancelLead}
        action={t.confirmCancel}
        busyAction={t.cancelling}
        onConfirm={() => act.mutateAsync('cancel')}
      />
    </li>
  )
}

function SentCopyLine({ change }: { change: MailChange }) {
  const c = strings.writing.mailbox.sentCopy
  if (change.state === 'failed') {
    return <span className="text-danger">{c.failed(errorText(change.last_error))}</span>
  }
  const state = change.state === 'superseded_by_server' ? 'applied' : change.state
  const text = state === 'held' || state === 'sending' || state === 'applied' ? c[state] : ''
  return text ? <span>{text}</span> : null
}
