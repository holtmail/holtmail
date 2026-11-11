import { applyItip, archivesQuery, type Invitation } from '@holtmail/api-client'
import { Badge, Button, cn } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowRightIcon, CalendarIcon, RepeatIcon } from 'lucide-react'

import { RsvpPanel, type RsvpTarget } from '../items/rsvp-panel.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatWhen, participantLabel, viewerZone } from '../lib/events.ts'
import { answerable } from '../lib/rsvp.ts'
import { canApplyAnswers } from '../lib/writing.ts'
import { strings } from '../strings.ts'
import { Row } from './row.tsx'

export function Invitations({
  invitations,
  messageId,
  archiveId,
}: {
  invitations: Invitation[]
  messageId: string
  archiveId: string
}) {
  if (invitations.length === 0) return null
  return (
    <ul aria-label={strings.invitation.shown} className="grid gap-2">
      {invitations.map((invitation) => (
        <li key={invitation.item_id}>
          <InvitationCard
            invitation={invitation}
            archiveId={archiveId}
            messageId={messageId}
            target={
              invitations.length === 1
                ? { kind: 'message', id: messageId }
                : { kind: 'item', id: invitation.item_id }
            }
          />
        </li>
      ))}
    </ul>
  )
}

const openClass =
  'flex items-center gap-1 text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

function InvitationCard({
  invitation,
  target,
  archiveId,
  messageId,
}: {
  invitation: Invitation
  target: RsvpTarget
  archiveId: string
  messageId: string
}) {
  const when = formatWhen(invitation.start, invitation.end, invitation.all_day, viewerZone())
  const method =
    (invitation.method && strings.invitation.methods[invitation.method]) || strings.invitation.event
  const answer = invitation.my_status
    ? (strings.invitation.answers[invitation.my_status] ?? invitation.my_status)
    : undefined
  return (
    <section
      aria-label={invitation.summary ?? method}
      className="grid gap-3 border border-border bg-bg-subtle p-4"
    >
      <header className="flex items-start gap-3">
        <CalendarIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-fg-subtle" />
        <div className="grid min-w-0 flex-1 gap-0.5">
          <span className="font-mono text-xs text-fg-subtle uppercase">{method}</span>
          <span
            className={cn(
              'text-base wrap-break-word text-fg',
              invitation.cancelled && 'text-fg-muted line-through',
            )}
          >
            {invitation.summary ?? strings.list.noSubject}
          </span>
        </div>
        {invitation.cancelled && (
          <Badge variant="outline" className="border-danger text-danger">
            {strings.invitation.cancelled}
          </Badge>
        )}
      </header>
      <dl className="grid gap-1 text-sm">
        <Row label={strings.item.when}>
          {when ? (
            <>
              <span className="text-fg">{when.local}</span>
              {when.original && (
                <span className="block font-mono text-xs text-fg-subtle">{when.original}</span>
              )}
            </>
          ) : (
            <span className="text-fg-subtle">{strings.invitation.noTime}</span>
          )}
          {invitation.recurring && (
            <span className="mt-0.5 flex items-center gap-1 text-xs text-fg-subtle">
              <RepeatIcon aria-hidden className="size-3" />
              {strings.invitation.repeats}
            </span>
          )}
        </Row>
        {invitation.location && (
          <Row label={strings.invitation.location}>{invitation.location}</Row>
        )}
        {invitation.organizer && (
          <Row label={strings.invitation.organizer}>
            {participantLabel({
              email: invitation.organizer.email ?? undefined,
              name: invitation.organizer.name ?? undefined,
            })}
          </Row>
        )}
        {answer && <Row label={strings.invitation.yourAnswer}>{answer}</Row>}
      </dl>
      {answerable(invitation) && <RsvpPanel target={target} archiveId={archiveId} />}
      {invitation.schedule_note === 'unauthenticated' && (
        <ApplyAnyway messageId={messageId} archiveId={archiveId} />
      )}
      <footer className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-3 text-xs">
        <span className="text-fg-subtle">
          {strings.invitation.updatedSince(invitation.updates_after)}
        </span>
        <span className="flex items-center gap-4">
          {invitation.group_id && (
            <Link
              to="/items/$itemId"
              params={{ itemId: invitation.item_id }}
              className="text-fg-subtle underline-offset-2 hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            >
              {strings.invitation.history}
            </Link>
          )}
          {invitation.group_id ? (
            <Link
              to="/events/$groupId"
              params={{ groupId: invitation.group_id }}
              className={openClass}
            >
              {strings.invitation.open}
              <ArrowRightIcon aria-hidden className="size-3" />
            </Link>
          ) : (
            <Link to="/items/$itemId" params={{ itemId: invitation.item_id }} className={openClass}>
              {strings.invitation.open}
              <ArrowRightIcon aria-hidden className="size-3" />
            </Link>
          )}
        </span>
      </footer>
    </section>
  )
}

/** An attendee's answer that failed the sender check, with the override for editors. */
function ApplyAnyway({ messageId, archiveId }: { messageId: string; archiveId: string }) {
  const t = strings.edits.rsvp.unverified
  const queryClient = useQueryClient()
  const archives = useQuery(archivesQuery)
  const archive = archives.data?.find((a) => a.id === archiveId)
  const apply = useMutation({
    mutationFn: () => applyItip(messageId),
    onSuccess: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: ['threads'] }),
        queryClient.invalidateQueries({ queryKey: ['messages'] }),
        queryClient.invalidateQueries({ queryKey: ['items'] }),
        queryClient.invalidateQueries({ queryKey: ['events'] }),
        queryClient.invalidateQueries({ queryKey: ['pim-changes'] }),
        queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'calendar'] }),
      ]),
  })
  const allowed = canApplyAnswers(archive)
  return (
    <div className="grid gap-2 border-t border-border pt-3 text-sm">
      <p className="text-fg-muted">{t.lead}</p>
      {!allowed ? null : apply.isSuccess ? (
        <p role="status" className="text-fg">
          {t.applied}
        </p>
      ) : (
        <Button
          variant="secondary"
          size="sm"
          className="justify-self-start"
          disabled={apply.isPending}
          onClick={() => apply.mutate()}
        >
          {apply.isPending ? t.applying : t.apply}
        </Button>
      )}
      {apply.isError && (
        <p role="alert" className="text-xs text-danger">
          {t.failed} {authErrorMessage(apply.error)}
        </p>
      )}
    </div>
  )
}
