import {
  type EventCopy,
  type EventDetail,
  type EventPerson,
  type EventVersion,
  eventQuery,
  itemVersionRawUrl,
  rsvpItemOptionsQuery,
} from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Badge, Button, cn, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link, useParams, useSearch } from '@tanstack/react-router'
import {
  ArrowLeftIcon,
  CalendarIcon,
  CircleAlertIcon,
  DownloadIcon,
  MailIcon,
  RepeatIcon,
} from 'lucide-react'
import { m } from 'motion/react'
import { type ReactNode, useMemo } from 'react'

import { Changes } from '../items/changes.tsx'
import { RsvpPanel } from '../items/rsvp-panel.tsx'
import { senderLabel } from '../lib/address.ts'
import { calendarColor } from '../lib/calendar.ts'
import { fieldLabel, formatWhen, participantLabel, viewerZone } from '../lib/events.ts'
import { formatFullDate } from '../lib/format.ts'
import { describeRecurrence } from '../lib/recurrence.ts'
import { tally } from '../lib/rsvp.ts'
import { EmptyState } from '../mail/empty-state.tsx'
import { strings } from '../strings.ts'

const t = strings.calendar.event

const linkClass =
  'text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

export function EventPage() {
  const { groupId } = useParams({ from: '/events/$groupId' })
  const { occ } = useSearch({ from: '/events/$groupId' })
  const zone = useMemo(() => viewerZone(), [])
  const event = useQuery(eventQuery(groupId, zone))
  const archiveId = event.data?.archive_id

  return (
    <main className="h-full overflow-y-auto bg-bg text-fg">
      <div className="flex h-12 items-center border-b border-border px-2">
        <Button variant="ghost" size="sm" asChild>
          {archiveId ? (
            <Link to="/a/$archiveId/calendar" params={{ archiveId }}>
              <ArrowLeftIcon />
              {t.back}
            </Link>
          ) : (
            <Link to="/">
              <ArrowLeftIcon />
              {strings.item.back}
            </Link>
          )}
        </Button>
      </div>
      {event.isError ? (
        <EmptyState icon={<CircleAlertIcon />} title={t.loadFailed}>
          {event.error.message}
        </EmptyState>
      ) : event.data ? (
        <m.article
          className="mx-auto grid max-w-4xl gap-10 px-6 py-8"
          initial={{ opacity: 0, y: distance.sm }}
          animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
        >
          <Summary event={event.data} zone={zone} />
          {!event.data.cancelled && (
            <RsvpPanel
              target={{ kind: 'item', id: event.data.item_id }}
              archiveId={event.data.archive_id}
              occurrence={occ}
            />
          )}
          <Attendees event={event.data} />
          <Copies event={event.data} />
          <Differences event={event.data} />
          <Mail event={event.data} />
          <Timeline event={event.data} />
        </m.article>
      ) : (
        <div aria-busy className="mx-auto grid max-w-4xl gap-4 px-6 py-8">
          <Skeleton className="h-8 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-1/3" />
        </div>
      )}
    </main>
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

function Section({
  id,
  title,
  aside,
  children,
}: {
  id: string
  title: string
  aside?: ReactNode
  children: ReactNode
}) {
  return (
    <section aria-labelledby={id} className="grid gap-3">
      <h2 id={id} className="flex items-baseline gap-2 text-base font-medium">
        {title}
        {aside && <span className="font-mono text-xs font-normal text-fg-subtle">{aside}</span>}
      </h2>
      {children}
    </section>
  )
}

function PersonName({ person, archiveId }: { person: EventPerson; archiveId: string }) {
  const label = participantLabel({
    email: person.email ?? undefined,
    name: person.name ?? undefined,
  })
  if (!person.person_id) return <span className="text-fg">{label}</span>
  return (
    <Link
      to="/a/$archiveId/people"
      params={{ archiveId }}
      search={{ p: person.person_id }}
      className={linkClass}
    >
      {label}
    </Link>
  )
}

function answerLabel(status: string | null | undefined): string | undefined {
  return status ? (strings.invitation.answers[status] ?? status) : undefined
}

function Summary({ event, zone }: { event: EventDetail; zone: string }) {
  const when = formatWhen(event.start, event.end, event.all_day, zone)
  const recurrence = event.recurrence
  const copies = event.copies.length
  return (
    <header className="grid gap-4">
      <div className="flex flex-wrap items-start gap-3">
        <CalendarIcon aria-hidden className="mt-1.5 size-5 shrink-0 text-fg-subtle" />
        <h1
          className={cn(
            'min-w-0 flex-1 text-2xl font-normal wrap-break-word',
            event.cancelled && 'text-fg-muted line-through',
          )}
        >
          {event.title || strings.calendar.untitled}
        </h1>
        {copies > 1 && (
          <Badge variant="outline" title={strings.calendar.copiesLabel(copies)}>
            {strings.calendar.copies(copies)}
          </Badge>
        )}
        {event.cancelled && (
          <Badge variant="outline" className="border-danger text-danger">
            {strings.calendar.cancelled}
          </Badge>
        )}
      </div>
      <dl className="grid text-sm">
        <Field label={t.when}>
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
        </Field>
        {recurrence && (
          <Field label={t.repeats}>
            <span className="flex items-center gap-1 text-fg">
              <RepeatIcon aria-hidden className="size-3 shrink-0 text-fg-subtle" />
              {describeRecurrence(recurrence)}
            </span>
            {recurrence.rule && (
              <span className="block font-mono text-xs text-fg-subtle">{recurrence.rule}</span>
            )}
            {recurrence.exdates.length > 0 && (
              <span className="block text-xs">
                {t.except}:{' '}
                {recurrence.exdates.map((d) => d.value.slice(0, 16).replace('T', ' ')).join(', ')}
              </span>
            )}
            {recurrence.rdates.length > 0 && (
              <span className="block text-xs">
                {t.extra}:{' '}
                {recurrence.rdates.map((d) => d.value.slice(0, 16).replace('T', ' ')).join(', ')}
              </span>
            )}
            {recurrence.changed.length > 0 && (
              <span className="mt-1 block text-xs">
                {t.changedInstances}:
                <ul className="grid">
                  {recurrence.changed.map((c) => (
                    <li
                      key={c.recurrence_id?.value ?? c.start?.value}
                      className="flex flex-wrap gap-x-2"
                    >
                      <span className="font-mono">{c.recurrence_id?.value.replace('T', ' ')}</span>
                      {c.cancelled ? (
                        <span className="text-danger">{strings.calendar.cancelled}</span>
                      ) : (
                        c.start && <span>{t.moved(c.start.value.replace('T', ' '))}</span>
                      )}
                    </li>
                  ))}
                </ul>
              </span>
            )}
          </Field>
        )}
        {event.location && <Field label={t.where}>{event.location}</Field>}
        {event.organizer && (
          <Field label={t.organizer}>
            <PersonName person={event.organizer} archiveId={event.archive_id} />
          </Field>
        )}
        {event.description && (
          <Field label={t.description}>
            <span className="whitespace-pre-wrap">{event.description}</span>
          </Field>
        )}
      </dl>
    </header>
  )
}

function copyLabel(copy: EventCopy): string {
  return copy.from_mail ? t.fromMail : copy.collection_name
}

function Attendees({ event }: { event: EventDetail }) {
  const options = useQuery(rsvpItemOptionsQuery(event.item_id))
  if (event.attendees.length === 0) return null
  const several = event.copies.length > 1
  const counts = options.data?.reason === 'organizer' ? tally(event.attendees) : undefined
  return (
    <Section id="event-attendees" title={t.attendees} aside={String(event.attendees.length)}>
      {counts && (
        <p className="text-sm text-fg-muted">
          {strings.edits.rsvp.tally(
            counts.accepted,
            counts.tentative,
            counts.declined,
            counts.none,
          )}
        </p>
      )}
      <div className="overflow-x-auto">
        <table className="w-full border-collapse text-sm">
          <thead>
            <tr className="text-left text-xs text-fg-subtle">
              <th className="py-1 pr-4 font-normal">{strings.landmarks.person}</th>
              {several ? (
                event.copies.map((copy) => (
                  <th key={copy.item_id} className="py-1 pr-4 font-normal">
                    {copyLabel(copy)}
                  </th>
                ))
              ) : (
                <th className="py-1 pr-4 font-normal">{t.answer}</th>
              )}
            </tr>
          </thead>
          <tbody>
            {event.attendees.map((person) => (
              <tr key={person.email ?? person.name} className="border-t border-border">
                <td className="py-1.5 pr-4">
                  <PersonName person={person} archiveId={event.archive_id} />
                </td>
                {several ? (
                  event.copies.map((copy) => {
                    const answer = person.answers.find((a) => a.item_id === copy.item_id)
                    return (
                      <td key={copy.item_id} className="py-1.5 pr-4 text-fg-muted">
                        {answerLabel(answer?.status) ?? t.none}
                      </td>
                    )
                  })
                ) : (
                  <td className="py-1.5 pr-4 text-fg-muted">
                    {answerLabel(person.status) ?? t.none}
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Section>
  )
}

function Copies({ event }: { event: EventDetail }) {
  return (
    <Section id="event-copies" title={t.copies} aside={String(event.copies.length)}>
      <ul className="grid">
        {event.copies.map((copy) => {
          const color = calendarColor(copy.color)
          return (
            <li
              key={copy.item_id}
              className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border py-2 text-sm"
            >
              <span
                aria-hidden
                className={cn(
                  'size-2.5 shrink-0 border border-border-strong',
                  !color && 'bg-fg-subtle',
                )}
                style={color ? { backgroundColor: color, borderColor: color } : undefined}
              />
              <span
                className={cn('text-fg', copy.removed_at != null && 'text-fg-muted line-through')}
              >
                {copyLabel(copy)}
              </span>
              {copy.account_name && (
                <span className="text-xs text-fg-subtle">{copy.account_name}</span>
              )}
              {copy.leading && (
                <span
                  title={t.leadingHint}
                  className="border border-border px-1 text-xs text-fg-subtle"
                >
                  {t.leading}
                </span>
              )}
              {copy.removed_at != null && (
                <span className="text-xs text-fg-subtle">{strings.calendar.removed}</span>
              )}
              <span className="ml-auto flex items-center gap-3 font-mono text-xs text-fg-subtle">
                {copy.sequence != null && <span>{t.sequence(copy.sequence)}</span>}
                <span>{t.versions(copy.versions)}</span>
                <Link to="/items/$itemId" params={{ itemId: copy.item_id }} className={linkClass}>
                  {t.openCopy}
                </Link>
              </span>
            </li>
          )
        })}
      </ul>
    </Section>
  )
}

function Differences({ event }: { event: EventDetail }) {
  if (event.copies.length < 2) return null
  return (
    <Section id="event-differences" title={t.differences}>
      {event.differences.length === 0 ? (
        <p className="text-sm text-fg-subtle">{t.noDifferences}</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="text-left text-xs text-fg-subtle">
                <th className="py-1 pr-4 font-normal">{t.field}</th>
                {event.copies.map((copy) => (
                  <th key={copy.item_id} className="py-1 pr-4 font-normal">
                    {copyLabel(copy)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {event.differences.map((difference) => (
                <tr key={difference.field} className="border-t border-border align-top">
                  <td className="py-1.5 pr-4 text-fg-subtle">{fieldLabel(difference.field)}</td>
                  {event.copies.map((copy) => {
                    const values =
                      difference.values.find((v) => v.item_id === copy.item_id)?.values ?? []
                    return (
                      <td key={copy.item_id} className="py-1.5 pr-4 wrap-break-word text-fg">
                        {values.length === 0 ? (
                          <span className="text-fg-subtle">{t.none}</span>
                        ) : (
                          <ul>
                            {values.map((value) => (
                              <li key={value}>{value}</li>
                            ))}
                          </ul>
                        )}
                      </td>
                    )
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Section>
  )
}

function Mail({ event }: { event: EventDetail }) {
  return (
    <Section
      id="event-mail"
      title={t.mail}
      aside={event.messages.length > 0 ? String(event.messages.length) : undefined}
    >
      {event.messages.length === 0 ? (
        <p className="text-sm text-fg-subtle">{t.noMail}</p>
      ) : (
        <ul className="grid">
          {event.messages.map((message) => (
            <li key={message.id} className="border-t border-border">
              <Link
                to="/a/$archiveId"
                params={{ archiveId: event.archive_id }}
                search={{ m: message.id }}
                className="flex items-baseline gap-3 py-1.5 text-sm outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
              >
                <MailIcon aria-hidden className="size-3 shrink-0 self-center text-fg-subtle" />
                <span className="min-w-0 flex-1 truncate text-fg">
                  {message.subject || strings.item.noSubject}
                </span>
                <span className="shrink-0 text-xs text-fg-subtle">
                  {senderLabel(message.from, strings.list.noSender)}
                </span>
                <time
                  dateTime={new Date(message.date).toISOString()}
                  className="shrink-0 font-mono text-xs text-fg-subtle tabular-nums"
                >
                  {formatFullDate(message.date)}
                </time>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </Section>
  )
}

function Timeline({ event }: { event: EventDetail }) {
  const versions = event.timeline.toReversed()
  return (
    <Section
      id="event-history"
      title={t.timeline}
      aside={strings.item.versions(event.timeline.length)}
    >
      <ol className="border-l border-border">
        {versions.map((version) => (
          <li
            key={`${version.item_id}-${version.number}`}
            className="grid gap-2 pb-6 pl-5 last:pb-0"
          >
            <VersionEntry version={version} several={event.copies.length > 1} />
          </li>
        ))}
      </ol>
    </Section>
  )
}

function VersionEntry({ version, several }: { version: EventVersion; several: boolean }) {
  const method = version.method
    ? (strings.invitation.methods[version.method] ?? version.method)
    : undefined
  return (
    <>
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        {several && <span className="text-sm text-fg">{version.collection_name}</span>}
        <span className="text-sm font-medium text-fg">{strings.item.version(version.number)}</span>
        {method && <span className="font-mono text-xs text-fg-subtle uppercase">{method}</span>}
        <time
          dateTime={new Date(version.at).toISOString()}
          className="font-mono text-xs text-fg-subtle tabular-nums"
        >
          {formatFullDate(version.at)}
        </time>
        <a
          href={itemVersionRawUrl(version.item_id, version.number)}
          download
          aria-label={strings.item.rawLabel(version.number)}
          className="ml-auto flex items-center gap-1 text-xs text-fg-subtle hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
        >
          <DownloadIcon aria-hidden className="size-3" />
          {strings.item.raw}
        </a>
      </div>
      <Changes number={version.number} readable={version.readable} changes={version.changes} />
    </>
  )
}
