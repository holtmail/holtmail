import {
  collectionsQuery,
  deleteItem,
  type ItemDetail,
  type ItemVersion,
  itemQuery,
  itemVersionRawUrl,
  type PimChange,
  type TimelineEntry,
  organizingQuery,
} from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Badge, Button, cn, Skeleton, Switch } from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useParams } from '@tanstack/react-router'
import {
  ArrowLeftIcon,
  CalendarIcon,
  CircleAlertIcon,
  ContactIcon,
  DownloadIcon,
  MailIcon,
  PencilIcon,
  PencilLineIcon,
  Trash2Icon,
} from 'lucide-react'
import { m } from 'motion/react'
import { type ReactNode, useId, useState } from 'react'

import { senderLabel } from '../lib/address.ts'
import {
  type EventView,
  formatWhen,
  participantLabel,
  readEvent,
  viewerZone,
} from '../lib/events.ts'
import { formatFullDate } from '../lib/format.ts'
import { type ContactView, readContact } from '../lib/people.ts'
import { canChooseInvitations, invitationsFlag, organizingNote } from '../lib/rsvp.ts'
import { EmptyState } from '../mail/empty-state.tsx'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { ChangeDiff } from './change-diff.tsx'
import { Changes } from './changes.tsx'
import { EditDialog } from './edit-dialog.tsx'
import { LocalEditMark } from './local-edit-mark.tsx'

const e = strings.edits

export function ItemPage() {
  const { itemId } = useParams({ from: '/items/$itemId' })
  const item = useQuery(itemQuery(itemId))
  const archiveId = item.data?.item.archive_id
  const contact = item.data?.item.kind === 'contact'

  return (
    <main className="h-full overflow-y-auto bg-bg text-fg">
      <div className="flex h-12 items-center border-b border-border px-2">
        <Button variant="ghost" size="sm" asChild>
          {archiveId && contact ? (
            <Link to="/a/$archiveId/people" params={{ archiveId }}>
              <ArrowLeftIcon />
              {strings.item.backToContacts}
            </Link>
          ) : archiveId ? (
            <Link to="/a/$archiveId" params={{ archiveId }}>
              <ArrowLeftIcon />
              {strings.item.back}
            </Link>
          ) : (
            <Link to="/">
              <ArrowLeftIcon />
              {strings.item.back}
            </Link>
          )}
        </Button>
      </div>
      {item.isError ? (
        <EmptyState icon={<CircleAlertIcon />} title={strings.item.loadFailed}>
          {item.error.message}
        </EmptyState>
      ) : item.data ? (
        <m.article
          className="mx-auto grid max-w-3xl gap-8 px-6 py-8"
          initial={{ opacity: 0, y: distance.sm }}
          animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
        >
          <ItemActions detail={item.data} />
          <ItemSummary detail={item.data} />
          <Timeline detail={item.data} />
        </m.article>
      ) : (
        <div aria-busy className="mx-auto grid max-w-3xl gap-4 px-6 py-8">
          <Skeleton className="h-8 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-1/3" />
        </div>
      )}
    </main>
  )
}

function ItemSummary({ detail }: { detail: ItemDetail }) {
  const contact = readContact(detail.data)
  if (contact) return <ContactSummary contact={contact} title={detail.item.title} />
  const event = readEvent(detail.data)
  const cancelled = event?.status === 'cancelled'
  return (
    <header className="grid gap-4">
      <div className="flex items-start gap-3">
        <CalendarIcon aria-hidden className="mt-1.5 size-5 shrink-0 text-fg-subtle" />
        <h1
          className={cn(
            'min-w-0 flex-1 text-2xl font-normal wrap-break-word',
            cancelled && 'text-fg-muted line-through',
          )}
        >
          {event?.summary ?? detail.item.title ?? strings.list.noSubject}
        </h1>
        {cancelled && (
          <Badge variant="outline" className="border-danger text-danger">
            {strings.invitation.cancelled}
          </Badge>
        )}
      </div>
      {event ? (
        <EventFields event={event} />
      ) : (
        <p className="text-sm text-fg-subtle">{strings.item.unreadable}</p>
      )}
    </header>
  )
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3 border-t border-border py-2">
      <dt className="w-28 shrink-0 text-fg-subtle">{label}</dt>
      <dd className="min-w-0 flex-1 text-fg-muted wrap-break-word">{children}</dd>
    </div>
  )
}

function ContactSummary({ contact, title }: { contact: ContactView; title?: string | null }) {
  return (
    <header className="grid gap-4">
      <div className="flex items-start gap-3">
        <ContactIcon aria-hidden className="mt-1.5 size-5 shrink-0 text-fg-subtle" />
        <h1 className="min-w-0 flex-1 text-2xl font-normal wrap-break-word">
          {contact.name ?? title ?? strings.people.noName}
        </h1>
      </div>
      <dl className="grid text-sm">
        {contact.emails.length > 0 && (
          <Field label={strings.item.emails}>
            <ul className="grid gap-0.5 font-mono text-xs">
              {contact.emails.map((email) => (
                <li key={email}>{email}</li>
              ))}
            </ul>
          </Field>
        )}
        {contact.phones.length > 0 && (
          <Field label={strings.item.phones}>
            <ul className="grid gap-0.5 font-mono text-xs">
              {contact.phones.map((phone) => (
                <li key={phone}>{phone}</li>
              ))}
            </ul>
          </Field>
        )}
        {contact.organization && (
          <Field label={strings.item.organization}>{contact.organization}</Field>
        )}
        {contact.title && <Field label={strings.item.jobTitle}>{contact.title}</Field>}
        {contact.birthday && <Field label={strings.item.birthday}>{contact.birthday}</Field>}
        {contact.note && (
          <Field label={strings.item.note}>
            <span className="whitespace-pre-wrap">{contact.note}</span>
          </Field>
        )}
      </dl>
    </header>
  )
}

function EventFields({ event }: { event: EventView }) {
  const when = formatWhen(event.start, event.end, event.allDay, viewerZone())
  return (
    <dl className="grid text-sm">
      <Field label={strings.item.when}>
        {when ? (
          <>
            <span className="text-fg">{when.local}</span>
            {when.original && (
              <span className="block font-mono text-xs text-fg-subtle">{when.original}</span>
            )}
          </>
        ) : (
          strings.invitation.noTime
        )}
        {event.recurring && (
          <span className="block text-xs text-fg-subtle">{strings.invitation.repeats}</span>
        )}
      </Field>
      {event.location && <Field label={strings.invitation.location}>{event.location}</Field>}
      {event.organizer && (
        <Field label={strings.invitation.organizer}>{participantLabel(event.organizer)}</Field>
      )}
      {event.attendees.length > 0 && (
        <Field label={strings.item.attendees}>
          <ul className="grid gap-0.5">
            {event.attendees.map((attendee) => (
              <li key={attendee.email ?? attendee.name} className="flex flex-wrap gap-x-2">
                <span className="text-fg">{participantLabel(attendee)}</span>
                {attendee.status && (
                  <span className="text-fg-subtle">
                    {strings.invitation.answers[attendee.status] ?? attendee.status}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </Field>
      )}
      {event.description && (
        <Field label={strings.item.description}>
          <span className="whitespace-pre-wrap">{event.description}</span>
        </Field>
      )}
    </dl>
  )
}

function ItemActions({ detail }: { detail: ItemDetail }) {
  const item = detail.item
  const collections = useQuery(collectionsQuery(item.archive_id)).data
  const editable = collections?.find((c) => c.id === item.collection_id)?.editable === true
  const [editing, setEditing] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const [tellAttendees, setTellAttendees] = useState(true)
  const queryClient = useQueryClient()
  const removed = item.removed_at != null && !item.local_edit
  const tellId = useId()
  const organizing = useQuery({
    ...organizingQuery(item.collection_id, item.id),
    enabled: deleting && item.kind === 'event',
  }).data
  const hasAttendees = (readEvent(detail.data)?.attendees.length ?? 0) > 0
  const tells = hasAttendees && organizing !== undefined && organizing.route !== 'none'
  if (!item.local_edit && (!editable || removed)) return null
  const contact = item.kind === 'contact'
  return (
    <div className="flex flex-wrap items-center gap-2">
      {item.local_edit && <LocalEditMark edit={item.local_edit} />}
      {editable && !removed && (item.kind === 'contact' || item.kind === 'event') && (
        <span className="ml-auto flex gap-1">
          <Button variant="secondary" size="sm" onClick={() => setEditing(true)}>
            <PencilIcon />
            {e.actions.edit}
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setDeleting(true)}>
            <Trash2Icon />
            {e.actions.delete}
          </Button>
        </span>
      )}
      {editing && (
        <EditDialog
          target={{ mode: 'edit', archiveId: item.archive_id, itemId: item.id }}
          onClose={() => setEditing(false)}
        />
      )}
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={e.remove.title}
        description={
          contact ? (
            e.remove.contact
          ) : (
            <span className="grid gap-3">
              <span>{e.remove.event}</span>
              {tells && (
                <span className="grid gap-2">
                  {canChooseInvitations(organizing) && (
                    <span className="flex items-center gap-3">
                      <Switch
                        id={tellId}
                        checked={tellAttendees}
                        onCheckedChange={setTellAttendees}
                      />
                      <label htmlFor={tellId}>{e.remove.keep}</label>
                    </span>
                  )}
                  <span className="text-xs">
                    {canChooseInvitations(organizing) && !tellAttendees
                      ? e.remove.silent
                      : organizing.route === 'unknown' || !organizing.allowed
                        ? organizingNote(organizing)
                        : e.remove.cancel(organizingNote(organizing))}
                  </span>
                </span>
              )}
            </span>
          )
        }
        action={e.remove.confirm}
        busyAction={e.dialog.saving}
        onConfirm={async () => {
          const current =
            organizing ??
            (hasAttendees
              ? await queryClient
                  .fetchQuery(organizingQuery(item.collection_id, item.id))
                  .catch(() => undefined)
              : undefined)
          const invitations = invitationsFlag(current, tellAttendees)
          await deleteItem(item.id, invitations === false ? { invitations: false } : {})
          await Promise.all([
            queryClient.invalidateQueries({ queryKey: ['items'] }),
            queryClient.invalidateQueries({ queryKey: ['archives', item.archive_id] }),
            queryClient.invalidateQueries({ queryKey: ['pim-changes'] }),
            queryClient.invalidateQueries({ queryKey: ['people'] }),
          ])
        }}
      />
    </div>
  )
}

function Timeline({ detail }: { detail: ItemDetail }) {
  const versions = new Map(detail.versions.map((v) => [v.number, v]))
  const entries: TimelineEntry[] =
    detail.timeline.length > 0
      ? detail.timeline.toReversed()
      : detail.versions
          .toReversed()
          .map((v) => ({ kind: 'version', at: v.seen_at, number: v.number }))
  return (
    <section aria-labelledby="item-history" className="grid gap-3">
      <h2 id="item-history" className="flex items-baseline gap-2 text-base font-medium">
        {strings.item.history}
        <span className="font-mono text-xs font-normal text-fg-subtle">
          {strings.item.versions(detail.versions.length)}
        </span>
      </h2>
      <ol className="border-l border-border">
        {entries.map((entry) => {
          const version = entry.number != null ? versions.get(entry.number) : undefined
          if (entry.kind === 'version' && version) {
            return (
              <li key={`v${version.number}`} className="grid gap-2 pb-6 pl-5 last:pb-0">
                <VersionEntry
                  itemId={detail.item.id}
                  archiveId={detail.item.archive_id}
                  version={version}
                  writeback={entry.origin === 'writeback'}
                />
              </li>
            )
          }
          if (entry.kind === 'change' && entry.change) {
            return (
              <li key={`c${entry.change.id}`} className="grid gap-2 pb-6 pl-5 last:pb-0">
                <ChangeEntry change={entry.change} />
              </li>
            )
          }
          return null
        })}
      </ol>
    </section>
  )
}

function ChangeEntry({ change }: { change: PimChange }) {
  const [open, setOpen] = useState(change.state === 'conflict')
  return (
    <>
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <PencilLineIcon aria-hidden className="size-3.5 self-center text-fg-subtle" />
        <span className="text-sm font-medium text-fg">{e.timeline.change(change.origin)}</span>
        <span className="text-xs text-fg-subtle">
          {e.classes[change.class] ?? change.class} · {e.states[change.state] ?? change.state}
        </span>
        <time
          dateTime={new Date(change.created_at).toISOString()}
          className="font-mono text-xs text-fg-subtle tabular-nums"
        >
          {formatFullDate(change.created_at)}
        </time>
        <button
          type="button"
          aria-expanded={open}
          className="ml-auto text-xs text-fg-subtle underline-offset-2 hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
          onClick={() => setOpen((v) => !v)}
        >
          {open ? e.actions.hideDiff : e.actions.showDiff}
        </button>
      </div>
      {change.reason && (
        <p className="text-sm text-fg-muted">{e.reasons[change.reason] ?? change.reason}</p>
      )}
      {open && <ChangeDiff changeId={change.id} />}
    </>
  )
}

function VersionEntry({
  itemId,
  archiveId,
  version,
  writeback,
}: {
  itemId: string
  archiveId: string
  version: ItemVersion
  writeback: boolean
}) {
  const method = version.method
    ? (strings.invitation.methods[version.method] ?? version.method)
    : undefined
  const when = version.messages[0]?.date ?? version.seen_at
  return (
    <>
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-sm font-medium text-fg">{strings.item.version(version.number)}</span>
        {method && <span className="font-mono text-xs text-fg-subtle uppercase">{method}</span>}
        {writeback && <span className="text-xs text-fg-subtle">{e.timeline.writeback}</span>}
        <time
          dateTime={new Date(when).toISOString()}
          className="font-mono text-xs text-fg-subtle tabular-nums"
        >
          {formatFullDate(when)}
        </time>
        <a
          href={itemVersionRawUrl(itemId, version.number)}
          download
          aria-label={strings.item.rawLabel(version.number)}
          className="ml-auto flex items-center gap-1 text-xs text-fg-subtle hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
        >
          <DownloadIcon aria-hidden className="size-3" />
          {strings.item.raw}
        </a>
      </div>
      <Changes number={version.number} readable={version.readable} changes={version.changes} />
      {version.messages.length > 0 && (
        <ul aria-label={strings.item.foundIn} className="grid gap-0.5">
          {version.messages.map((message) => (
            <li key={message.id}>
              <Link
                to="/a/$archiveId"
                params={{ archiveId }}
                search={{ m: message.id }}
                className="flex items-baseline gap-2 text-sm text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
              >
                <MailIcon aria-hidden className="size-3 shrink-0 self-center" />
                <span className="truncate">{message.subject || strings.item.noSubject}</span>
                <span className="shrink-0 text-xs text-fg-subtle">
                  {senderLabel(message.from, strings.list.noSender)}
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </>
  )
}
