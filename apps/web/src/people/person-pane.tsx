import {
  type Correspondence,
  type Meeting,
  type PersonCard,
  type PersonDetail,
  type PersonField,
  type PersonLink,
  personQuery,
  type ValueSource,
} from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Button, cn, Kbd, Skeleton, Tooltip, TooltipContent, TooltipTrigger } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import {
  ArrowLeftIcon,
  CalendarIcon,
  CircleAlertIcon,
  ContactIcon,
  MailIcon,
  UsersIcon,
} from 'lucide-react'
import { m } from 'motion/react'
import type { ReactNode } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { formatCount, formatFullDate, formatListDate } from '../lib/format.ts'
import {
  differingFields,
  initials,
  linkReason,
  personFieldLabel,
  personLabel,
  seenLabel,
  sourceLabel,
  yearBars,
} from '../lib/people.ts'
import { Attachments } from '../mail/attachments.tsx'
import { ids } from '../mail/context.ts'
import { EmptyState } from '../mail/empty-state.tsx'
import { strings } from '../strings.ts'
import { OverrideNotes, PersonActions } from './person-actions.tsx'

const t = strings.people

const linkClass =
  'text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

export function PeopleEmpty() {
  return (
    <EmptyState icon={<UsersIcon />} title={t.noneSelected}>
      {t.noneSelectedHint} <Kbd>J</Kbd> <Kbd>K</Kbd> {strings.reader.toOpen}
    </EmptyState>
  )
}

export function PersonPane({ personId, onBack }: { personId: string; onBack?: () => void }) {
  const person = useQuery(personQuery(personId))
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-12 shrink-0 items-center gap-1 border-b border-border px-2">
        {onBack && (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={t.back} onClick={onBack}>
                <ArrowLeftIcon />
              </Button>
            </TooltipTrigger>
            <TooltipContent>{t.back}</TooltipContent>
          </Tooltip>
        )}
      </div>
      <div id={ids.reader} tabIndex={-1} className="min-h-0 flex-1 overflow-y-auto outline-none">
        {person.isError ? (
          <EmptyState icon={<CircleAlertIcon />} title={t.personFailed}>
            {person.error.message}
          </EmptyState>
        ) : person.data ? (
          <m.article
            key={personId}
            className="grid gap-8 px-6 pt-6 pb-10"
            initial={{ opacity: 0, y: distance.sm }}
            animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
          >
            <Header detail={person.data} />
            <Fields fields={person.data.fields} mailOnly={person.data.person.cards === 0} />
            <Cards cards={person.data.cards} links={person.data.links} />
            <CorrespondenceSection
              archiveId={person.data.person.archive_id}
              mail={person.data.correspondence}
            />
            <Meetings meetings={person.data.meetings} />
          </m.article>
        ) : (
          <div aria-busy className="grid gap-6 px-6 pt-6">
            <div className="flex items-center gap-4">
              <Skeleton className="size-12" />
              <div className="grid flex-1 gap-2">
                <Skeleton className="h-6 w-1/2" />
                <Skeleton className="h-4 w-1/3" />
              </div>
            </div>
            <Skeleton className="h-4 w-2/3" />
            <Skeleton className="h-4 w-1/2" />
          </div>
        )}
      </div>
    </div>
  )
}

function Header({ detail }: { detail: PersonDetail }) {
  const { person } = detail
  const label = personLabel(person, t.noName)
  const letters = initials(person.name ?? '')
  const differing = differingFields(detail.fields)
  return (
    <header className="grid gap-3">
      <div className="flex items-start gap-4">
        <span
          aria-hidden
          className="flex size-12 shrink-0 items-center justify-center border border-border bg-bg-subtle text-base font-medium text-fg-muted [&_svg]:size-5"
        >
          {letters || <UsersIcon />}
        </span>
        <div className="grid min-w-0 flex-1 gap-1">
          <h2
            className={cn(
              'text-2xl font-normal wrap-break-word text-fg',
              person.removed && 'text-fg-muted line-through',
            )}
          >
            {label}
          </h2>
          <p className="flex flex-wrap gap-x-4 gap-y-1 font-mono text-xs text-fg-subtle">
            {person.email && person.email !== label && (
              <a href={`mailto:${person.email}`} className={linkClass}>
                {person.email}
              </a>
            )}
            {person.phone && person.phone !== label && (
              <a href={`tel:${person.phone}`} className={linkClass}>
                {person.phone}
              </a>
            )}
          </p>
        </div>
      </div>
      <ul className="flex flex-wrap gap-2 text-xs text-fg-muted">
        {person.cards > 0 ? (
          <Mark icon={<ContactIcon />}>{t.cards(person.cards)}</Mark>
        ) : (
          <Mark icon={<MailIcon />}>{t.mailOnly}</Mark>
        )}
        {person.removed && <Mark>{t.removed}</Mark>}
        {person.differences && <Mark>≠ {t.differs}</Mark>}
      </ul>
      {differing.length > 0 && (
        <p className="text-sm text-fg-muted">{t.differsSummary(differing.join(', '))}</p>
      )}
      <OverrideNotes detail={detail} />
      <PersonActions detail={detail} />
    </header>
  )
}

function Mark({ icon, children }: { icon?: ReactNode; children: ReactNode }) {
  return (
    <li className="flex h-6 items-center gap-1 border border-border px-2 [&_svg]:size-3">
      {icon}
      {children}
    </li>
  )
}

function Section({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <section aria-labelledby={id} className="grid gap-3">
      <h3 id={id} className="text-base font-medium">
        {title}
      </h3>
      {children}
    </section>
  )
}

function Fields({ fields, mailOnly }: { fields: PersonField[]; mailOnly: boolean }) {
  const now = useNow(60_000)
  if (mailOnly) return <p className="text-sm text-fg-muted">{t.mailOnlyNote}</p>
  if (fields.length === 0) return null
  return (
    <Section id="person-fields" title={t.fields}>
      <dl className="grid text-sm">
        {fields.map((field) => (
          <div key={field.name} className="flex gap-3 border-t border-border py-2">
            <dt className="flex w-28 shrink-0 items-baseline gap-1 text-fg-subtle">
              {personFieldLabel(field.name)}
              {field.differs && (
                <span title={t.differsNote} className="font-mono text-fg-muted">
                  <span aria-hidden>≠</span>
                  <span className="sr-only">{t.differsNote}</span>
                </span>
              )}
            </dt>
            <dd className="grid min-w-0 flex-1 gap-1">
              {field.values.map((value) => (
                <div key={value.value} className="flex flex-wrap items-baseline gap-x-2">
                  <span
                    className={cn(
                      'wrap-break-word whitespace-pre-wrap text-fg',
                      value.sources.every((s) => s.removed) && 'text-fg-muted line-through',
                    )}
                  >
                    {value.value}
                  </span>
                  {field.differs ? (
                    <span className="text-xs text-fg-subtle">
                      {value.sources
                        .map(
                          (source) =>
                            `${sourceLabel(source)}, ${seenLabel(source.seen_at, source.removed, now)}`,
                        )
                        .join(' · ')}
                    </span>
                  ) : (
                    <SourceMarks sources={value.sources} />
                  )}
                </div>
              ))}
            </dd>
          </div>
        ))}
      </dl>
    </Section>
  )
}

function SourceMarks({ sources }: { sources: ValueSource[] }) {
  const names = [...new Set(sources.map(sourceLabel))]
  return (
    <span className="flex flex-wrap gap-1" aria-label={t.agreeSources(names.join(', '))}>
      {names.map((name) => (
        <span
          key={name}
          aria-hidden
          className="max-w-40 truncate border border-border px-1 text-xs leading-4 text-fg-subtle"
        >
          {name}
        </span>
      ))}
    </span>
  )
}

function Cards({ cards, links }: { cards: PersonCard[]; links: PersonLink[] }) {
  const now = useNow(60_000)
  if (cards.length === 0) return null
  const title = (id: string) => {
    const card = cards.find((c) => c.item_id === id)
    return card ? (card.title ?? sourceLabel(card)) : id
  }
  return (
    <Section id="person-cards" title={t.cardsHeading}>
      <ul className="grid">
        {cards.map((card) => (
          <li key={card.item_id} className="border-t border-border py-2">
            <Link
              to="/items/$itemId"
              params={{ itemId: card.item_id }}
              aria-label={t.openCard(card.title ?? sourceLabel(card))}
              className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm outline-none hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            >
              <span
                className={cn(
                  'text-accent underline-offset-2 hover:underline',
                  card.removed_at != null && 'line-through',
                )}
              >
                {card.title ?? t.noName}
              </span>
              <span className="text-xs text-fg-subtle">{sourceLabel(card)}</span>
              <span className="ml-auto flex gap-3 font-mono text-xs text-fg-subtle tabular-nums">
                {!card.readable && <span>{t.unreadable}</span>}
                <span>{t.versions(card.versions)}</span>
                <span>{seenLabel(card.seen_at, card.removed_at != null, now)}</span>
              </span>
            </Link>
          </li>
        ))}
      </ul>
      {links.length > 0 && (
        <div className="grid gap-1">
          <h4 className="text-xs text-fg-subtle">{t.why}</h4>
          <ul className="grid gap-0.5 text-sm text-fg-muted">
            {links.map((link) => (
              <li key={`${link.from_item_id}-${link.to_item_id}-${link.value}`}>
                <span className="font-mono text-xs">{linkReason(link)}</span>
                <span className="px-2 text-fg-subtle">
                  {title(link.from_item_id)} ↔ {title(link.to_item_id)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Section>
  )
}

function CorrespondenceSection({ archiveId, mail }: { archiveId: string; mail: Correspondence }) {
  const now = useNow()
  const bars = yearBars(mail.years)
  return (
    <Section id="person-mail" title={t.correspondence}>
      {mail.messages === 0 ? (
        <p className="text-sm text-fg-subtle">{t.noMail}</p>
      ) : (
        <>
          <p className="flex flex-wrap gap-x-4 gap-y-1 text-sm text-fg-muted">
            <span className="text-fg">{t.messages(formatCount(mail.messages))}</span>
            <span>{t.fromThem(formatCount(mail.from_them))}</span>
            <span>{t.toThem(formatCount(mail.to_them))}</span>
          </p>
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5 text-sm">
            {mail.first_at != null && (
              <>
                <dt className="text-fg-subtle">{t.firstMessage}</dt>
                <dd className="font-mono text-xs leading-5 text-fg-muted tabular-nums">
                  {formatFullDate(mail.first_at)}
                </dd>
              </>
            )}
            {mail.last_at != null && (
              <>
                <dt className="text-fg-subtle">{t.lastMessage}</dt>
                <dd className="font-mono text-xs leading-5 text-fg-muted tabular-nums">
                  {formatFullDate(mail.last_at)}
                </dd>
              </>
            )}
          </dl>
          {bars.length > 1 && (
            <figure className="grid gap-1">
              <figcaption className="text-xs text-fg-subtle">{t.perYear}</figcaption>
              <ul className="flex h-16 items-end gap-px border-b border-border">
                {bars.map((bar) => (
                  <li
                    key={bar.year}
                    title={t.yearCount(bar.year, formatCount(bar.messages))}
                    className="flex h-full min-w-1 flex-1 items-end"
                  >
                    <span className="sr-only">
                      {t.yearCount(bar.year, formatCount(bar.messages))}
                    </span>
                    <span
                      aria-hidden
                      className="block h-full w-full origin-bottom bg-border-strong"
                      style={{
                        transform: `scaleY(${Math.max(bar.share, bar.messages > 0 ? 0.04 : 0)})`,
                      }}
                    />
                  </li>
                ))}
              </ul>
              <div
                aria-hidden
                className="flex justify-between font-mono text-xs text-fg-subtle tabular-nums"
              >
                <span>{bars[0]?.year}</span>
                <span>{bars.at(-1)?.year}</span>
              </div>
            </figure>
          )}
          {mail.threads.length > 0 && (
            <div className="grid gap-1">
              <h4 className="text-xs text-fg-subtle">{t.threads}</h4>
              <ul className="grid">
                {mail.threads.map((thread) => (
                  <li key={thread.thread_id} className="border-t border-border">
                    <Link
                      to="/a/$archiveId"
                      params={{ archiveId }}
                      search={{ m: thread.message_id }}
                      className="flex items-baseline gap-3 py-1.5 text-sm outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
                    >
                      <MailIcon
                        aria-hidden
                        className="size-3 shrink-0 self-center text-fg-subtle"
                      />
                      <span className="min-w-0 flex-1 truncate text-fg">
                        {thread.subject || strings.list.noSubject}
                      </span>
                      <span className="shrink-0 font-mono text-xs text-fg-subtle tabular-nums">
                        {t.inThread(thread.messages)}
                      </span>
                      <time
                        dateTime={new Date(thread.last_at).toISOString()}
                        className="w-16 shrink-0 text-right font-mono text-xs text-fg-subtle tabular-nums"
                      >
                        {formatListDate(thread.last_at, now)}
                      </time>
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <Attachments
            label={t.attachments}
            attachments={mail.attachments.map((item) => item.attachment)}
          />
        </>
      )}
    </Section>
  )
}

function meetingWhen(meeting: Meeting): string | undefined {
  if (meeting.starts_at == null) return undefined
  const date = new Date(meeting.starts_at)
  return meeting.all_day
    ? new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' }).format(date)
    : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date)
}

const meetingClass =
  'flex flex-wrap items-baseline gap-x-3 gap-y-0.5 py-1.5 text-sm outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus'

function MeetingLink({ meeting, children }: { meeting: Meeting; children: ReactNode }) {
  return meeting.group_id ? (
    <Link to="/events/$groupId" params={{ groupId: meeting.group_id }} className={meetingClass}>
      {children}
    </Link>
  ) : (
    <Link to="/items/$itemId" params={{ itemId: meeting.item_id }} className={meetingClass}>
      {children}
    </Link>
  )
}

function Meetings({ meetings }: { meetings: Meeting[] }) {
  return (
    <Section id="person-meetings" title={t.meetings}>
      {meetings.length === 0 ? (
        <p className="text-sm text-fg-subtle">{t.noMeetings}</p>
      ) : (
        <ul className="grid">
          {meetings.map((meeting) => {
            const when = meetingWhen(meeting)
            const answer = meeting.status
              ? (strings.invitation.answers[meeting.status] ?? meeting.status)
              : undefined
            return (
              <li key={meeting.item_id} className="border-t border-border">
                <MeetingLink meeting={meeting}>
                  <CalendarIcon
                    aria-hidden
                    className="size-3 shrink-0 self-center text-fg-subtle"
                  />
                  <span
                    className={cn(
                      'min-w-0 flex-1 truncate text-fg',
                      (meeting.cancelled || meeting.removed_at != null) &&
                        'text-fg-muted line-through',
                    )}
                  >
                    {meeting.title || strings.list.noSubject}
                  </span>
                  <span className="text-xs text-fg-subtle">
                    {meeting.role === 'organizer' ? t.organizer : (answer ?? t.attendee)}
                    {meeting.cancelled && ` · ${t.cancelled}`}
                  </span>
                  {when && (
                    <span className="font-mono text-xs text-fg-subtle tabular-nums">{when}</span>
                  )}
                </MeetingLink>
              </li>
            )
          })}
        </ul>
      )}
    </Section>
  )
}
