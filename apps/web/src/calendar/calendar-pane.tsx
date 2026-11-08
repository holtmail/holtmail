import {
  type CalendarOccurrence,
  type CalendarSource,
  calendarExportUrl,
  calendarQuery,
  collectionsQuery,
} from '@holtmail/api-client'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Skeleton,
  ToggleGroup,
  ToggleGroupItem,
} from '@holtmail/ui'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import {
  CalendarDaysIcon,
  CalendarIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  DownloadIcon,
  FileArchiveIcon,
  FileTextIcon,
  LayersIcon,
} from 'lucide-react'
import {
  type CSSProperties,
  Fragment,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'

import { useHotkeys } from '../hooks/use-hotkeys.ts'
import { useNow } from '../hooks/use-now.ts'
import { NewItemButton } from '../items/new-item-button.tsx'
import {
  byDay,
  type CalendarView,
  calendarColor,
  calendarViews,
  type DayKey,
  isCalendarView,
  dayMinutes,
  layoutColumns,
  shiftAnchor,
  shownCalendars,
  todayKey,
  viewSpan,
  wallClock,
} from '../lib/calendar.ts'
import { viewerZone } from '../lib/events.ts'
import { calendarExportFormats } from '../lib/export.ts'
import type { CalendarSearch } from '../lib/search-params.ts'
import { useMail } from '../mail/context.ts'
import { EmptyState } from '../mail/empty-state.tsx'
import { strings } from '../strings.ts'

const t = strings.calendar

const calendarKeys = {
  today: ['t'],
  agenda: ['a'],
  week: ['w'],
  month: ['m'],
  previousPeriod: ['left'],
  nextPeriod: ['right'],
  down: ['down'],
  up: ['up'],
} as const satisfies Record<string, readonly string[]>

type CalendarKey = keyof typeof calendarKeys

const hourRem = 3

function occurrenceKey(o: CalendarOccurrence): string {
  return `${o.group_id}|${o.recurrence_id ?? ''}|${o.starts_at}`
}

function domId(key: string): string {
  return `cal-${key.replaceAll(/[^a-zA-Z0-9-]/g, '_')}`
}

function dateOf(day: DayKey): Date {
  return new Date(`${day}T00:00:00Z`)
}

function title(view: CalendarView, days: readonly DayKey[], anchor: DayKey): string {
  if (view === 'week') {
    const first = days[0] ?? anchor
    const last = days.at(-1) ?? anchor
    return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' }).formatRange(
      dateOf(first),
      dateOf(last),
    )
  }
  return new Intl.DateTimeFormat(undefined, {
    month: 'long',
    year: 'numeric',
    timeZone: 'UTC',
  }).format(dateOf(anchor))
}

function shortTime(minutes: number): string {
  const date = new Date(Date.UTC(2000, 0, 1, Math.floor(minutes / 60), minutes % 60))
  return new Intl.DateTimeFormat(undefined, { timeStyle: 'short', timeZone: 'UTC' }).format(date)
}

function timeLabel(o: CalendarOccurrence, day: DayKey, zone: string): string {
  if (o.all_day) return t.allDay
  const { start, end } = dayMinutes(o, day, zone)
  return end > start ? `${shortTime(start)}–${shortTime(Math.min(end, 1439))}` : shortTime(start)
}

export function CalendarPane() {
  const { archiveId, list } = useMail()
  const search = useSearch({ from: '/a/$archiveId/calendar' })
  const navigate = useNavigate()
  const zone = useMemo(() => viewerZone(), [])
  const now = useNow()
  const today = todayKey(zone, now)
  const view = search.view ?? 'agenda'
  const anchor = search.d ?? today
  const hidden = useMemo(() => search.off?.split(',') ?? [], [search.off])
  const span = useMemo(() => viewSpan(view, anchor, zone), [view, anchor, zone])
  const collections = useQuery(collectionsQuery(archiveId))
  const allCalendars = useMemo(
    () => (collections.data ?? []).filter((c) => c.kind === 'calendar'),
    [collections.data],
  )
  const shown = shownCalendars(allCalendars, hidden)
  const range = useQuery({
    ...calendarQuery(archiveId, {
      from: span.from,
      to: span.to,
      collections: shown,
      showRemoved: search.removed,
    }),
    enabled: hidden.length === 0 || collections.isSuccess,
    placeholderData: keepPreviousData,
  })
  const calendars = useMemo(() => range.data?.calendars ?? [], [range.data])
  const colors = useMemo(
    () => new Map(calendars.map((c) => [c.id, calendarColor(c.color)])),
    [calendars],
  )
  const occurrences = useMemo(() => range.data?.occurrences ?? [], [range.data])
  const days = useMemo(() => byDay(occurrences, span.days, zone), [occurrences, span.days, zone])
  const ordered = useMemo(() => {
    const seen = new Set<string>()
    const out: CalendarOccurrence[] = []
    for (const day of span.days) {
      for (const o of days.get(day) ?? []) {
        const key = occurrenceKey(o)
        if (!seen.has(key)) {
          seen.add(key)
          out.push(o)
        }
      }
    }
    return out
  }, [days, span.days])
  const [selected, setSelected] = useState<string | undefined>(undefined)

  const go = useCallback(
    (next: CalendarSearch) =>
      void navigate({
        to: '/a/$archiveId/calendar',
        params: { archiveId },
        search: (previous) => ({ ...previous, ...next }),
        replace: true,
      }),
    [navigate, archiveId],
  )

  const move = useCallback(
    (delta: number) => {
      if (ordered.length === 0) return
      const at = ordered.findIndex((o) => occurrenceKey(o) === selected)
      const fallback = ordered.findIndex((o) => o.ends_at > now)
      const from = at === -1 ? (fallback === -1 ? 0 : fallback) - (delta > 0 ? 1 : 0) : at
      const next = ordered[Math.min(Math.max(from + delta, 0), ordered.length - 1)]
      if (!next) return
      const key = occurrenceKey(next)
      setSelected(key)
      requestAnimationFrame(() => document.getElementById(domId(key))?.focus())
    },
    [ordered, selected, now],
  )

  const open = useCallback(() => {
    const current = ordered.find((o) => occurrenceKey(o) === selected)
    if (current) {
      void navigate({
        to: '/events/$groupId',
        params: { groupId: current.group_id },
        search: current.recurrence_id ? { occ: current.recurrence_id } : {},
      })
    }
  }, [ordered, selected, navigate])

  const body = useRef<HTMLDivElement>(null)
  useEffect(() => {
    list.current = { move, open, focus: () => body.current?.focus() }
    return () => {
      list.current = null
    }
  }, [list, move, open])

  useHotkeys<CalendarKey>(calendarKeys, (key) => {
    switch (key) {
      case 'today':
        go({ d: undefined })
        break
      case 'agenda':
      case 'week':
      case 'month':
        go({ view: key === 'agenda' ? undefined : key })
        break
      case 'previousPeriod':
        go({ d: shiftAnchor(view, anchor, -1) })
        break
      case 'nextPeriod':
        go({ d: shiftAnchor(view, anchor, 1) })
        break
      case 'down':
        move(1)
        break
      case 'up':
        move(-1)
        break
    }
  })

  const toggleCalendar = (id: string, show: boolean) => {
    const next = show ? hidden.filter((h) => h !== id) : [...hidden, id]
    go({ off: next.length > 0 ? next.join(',') : undefined })
  }

  const context: ViewContext = {
    zone,
    today,
    now,
    colors,
    selected,
    onSelect: setSelected,
    onPickDay: (day) => go({ view: 'week', d: day }),
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 py-3">
        <h1 className="mr-auto min-w-0 truncate text-lg font-medium">
          <span className="sr-only">{t.title}: </span>
          {title(view, span.days, anchor)}
        </h1>
        <div className="flex items-center">
          <Button size="sm" variant="secondary" onClick={() => go({ d: undefined })}>
            {t.today}
          </Button>
          <Button
            size="icon"
            variant="ghost"
            aria-label={t.previous(view)}
            onClick={() => go({ d: shiftAnchor(view, anchor, -1) })}
          >
            <ChevronLeftIcon />
          </Button>
          <Button
            size="icon"
            variant="ghost"
            aria-label={t.next(view)}
            onClick={() => go({ d: shiftAnchor(view, anchor, 1) })}
          >
            <ChevronRightIcon />
          </Button>
        </div>
        <label className="flex items-center">
          <span className="sr-only">{t.jump}</span>
          <Input
            type="date"
            value={anchor}
            className="h-8 w-36 font-mono text-xs"
            onChange={(event) => {
              if (event.target.value) go({ d: event.target.value })
            }}
          />
        </label>
        <ToggleGroup
          type="single"
          size="sm"
          aria-label={t.viewLabel}
          value={view}
          onValueChange={(value) => {
            if (isCalendarView(value)) go({ view: value === 'agenda' ? undefined : value })
          }}
        >
          {calendarViews.map((v) => (
            <ToggleGroupItem key={v} value={v}>
              {t.views[v]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        <NewItemButton archiveId={archiveId} kind="event" day={anchor} />
        <CalendarsMenu
          calendars={calendars.length > 0 ? calendars : []}
          hidden={hidden}
          showRemoved={Boolean(search.removed)}
          onToggle={toggleCalendar}
          onShowAll={() => go({ off: undefined })}
          onShowRemoved={(on) => go({ removed: on ? true : undefined })}
        />
        <CalendarExportMenu
          archiveId={archiveId}
          from={span.from}
          to={span.to}
          collection={shown?.length === 1 ? shown[0] : undefined}
          removed={Boolean(search.removed)}
        />
      </header>
      {range.data?.truncated && (
        <p role="status" className="border-b border-border px-4 py-2 text-xs text-warning">
          {t.truncated}
        </p>
      )}
      <div
        ref={body}
        tabIndex={-1}
        aria-busy={range.isFetching}
        aria-label={t.title}
        className="relative min-h-0 flex-1 outline-none"
      >
        {range.isPending ? (
          <div aria-busy className="grid gap-3 p-4">
            {['w-1/3', 'w-1/2', 'w-2/5', 'w-1/4'].map((width, i) => (
              <Skeleton key={i} className={cn('h-4', width)} />
            ))}
          </div>
        ) : range.isError ? (
          <EmptyState icon={<CircleAlertIcon />} title={t.loadFailed}>
            <p>{range.error.message}</p>
            <Button size="sm" className="mt-4" onClick={() => void range.refetch()}>
              {strings.list.retry}
            </Button>
          </EmptyState>
        ) : calendars.length === 0 ? (
          <EmptyState icon={<CalendarIcon />} title={t.noCalendarsTitle}>
            <p>{t.noCalendarsLead}</p>
          </EmptyState>
        ) : view === 'agenda' ? (
          <AgendaView days={span.days} byDay={days} context={context} />
        ) : view === 'week' ? (
          <WeekView days={span.days} byDay={days} context={context} />
        ) : (
          <MonthView days={span.days} byDay={days} anchor={anchor} context={context} />
        )}
      </div>
    </div>
  )
}

interface ViewContext {
  zone: string
  today: DayKey
  now: number
  colors: Map<string, string | undefined>
  selected: string | undefined
  onSelect: (key: string) => void
  onPickDay: (day: DayKey) => void
}

function CalendarExportMenu({
  archiveId,
  from,
  to,
  collection,
  removed,
}: {
  archiveId: string
  from: number
  to: number
  collection?: string
  removed: boolean
}) {
  const scopes = [
    { key: 'shown', scope: { from, to, collection, removed } },
    { key: 'all', scope: { removed } },
  ]
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t.exportList} title={t.exportList}>
          <DownloadIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        {scopes.map(({ key, scope }, index) => (
          <Fragment key={key}>
            {index > 0 && <DropdownMenuSeparator />}
            <DropdownMenuLabel className="text-xs font-normal text-fg-subtle">
              {t.exportWhat[key]}
            </DropdownMenuLabel>
            {calendarExportFormats.map((format) => {
              const Icon = format === 'zip' ? FileArchiveIcon : FileTextIcon
              return (
                <DropdownMenuItem key={format} asChild>
                  <a href={calendarExportUrl(archiveId, format, scope)} download>
                    <Icon />
                    <span className="grid">
                      <span>{t.exportFormat[format]}</span>
                      <span className="text-xs text-fg-subtle">{t.exportHint[format]}</span>
                    </span>
                  </a>
                </DropdownMenuItem>
              )
            })}
          </Fragment>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function CalendarsMenu({
  calendars,
  hidden,
  showRemoved,
  onToggle,
  onShowAll,
  onShowRemoved,
}: {
  calendars: CalendarSource[]
  hidden: readonly string[]
  showRemoved: boolean
  onToggle: (id: string, show: boolean) => void
  onShowAll: () => void
  onShowRemoved: (on: boolean) => void
}) {
  const filtered = hidden.length > 0 || showRemoved
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="sm" className="relative">
          <LayersIcon />
          {t.calendars}
          {filtered && <span aria-hidden className="absolute top-1 right-1 size-1.5 bg-accent" />}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-72">
        <DropdownMenuLabel>{t.calendars}</DropdownMenuLabel>
        {calendars
          .filter((c) => showRemoved || c.removed_at == null)
          .map((c) => (
            <DropdownMenuCheckboxItem
              key={c.id}
              checked={!hidden.includes(c.id)}
              onCheckedChange={(checked) => onToggle(c.id, checked)}
              onSelect={(event) => event.preventDefault()}
            >
              <Swatch color={calendarColor(c.color)} />
              <span className="min-w-0 flex-1 truncate">{c.from_mail ? t.fromMail : c.name}</span>
              {c.account_name && (
                <span className="max-w-24 truncate text-xs text-fg-subtle">{c.account_name}</span>
              )}
            </DropdownMenuCheckboxItem>
          ))}
        <DropdownMenuSeparator />
        <DropdownMenuCheckboxItem
          checked={showRemoved}
          onCheckedChange={(checked) => onShowRemoved(checked)}
        >
          {t.showRemoved}
        </DropdownMenuCheckboxItem>
        {hidden.length > 0 && (
          <DropdownMenuItem onSelect={onShowAll}>{t.allCalendars}</DropdownMenuItem>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function Swatch({ color, className }: { color: string | undefined; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        'size-2.5 shrink-0 border border-border-strong',
        !color && 'bg-fg-subtle',
        className,
      )}
      style={color ? { backgroundColor: color, borderColor: color } : undefined}
    />
  )
}

function Marks({ o }: { o: CalendarOccurrence }) {
  return (
    <>
      {o.copies > 1 && (
        <span
          title={t.copiesLabel(o.copies)}
          className="shrink-0 border border-border px-1 font-mono text-[0.625rem] leading-4 text-fg-subtle"
        >
          {t.copies(o.copies)}
        </span>
      )}
      {o.cancelled && <span className="shrink-0 text-xs text-danger">{t.cancelled}</span>}
      {o.removed && <span className="shrink-0 text-xs text-fg-subtle">{t.removed}</span>}
    </>
  )
}

function EventLink({
  o,
  context,
  className,
  style,
  children,
}: {
  o: CalendarOccurrence
  context: ViewContext
  className?: string
  style?: CSSProperties
  children: ReactNode
}) {
  const key = occurrenceKey(o)
  return (
    <Link
      id={domId(key)}
      to="/events/$groupId"
      params={{ groupId: o.group_id }}
      search={o.recurrence_id ? { occ: o.recurrence_id } : {}}
      data-selected={context.selected === key ? '' : undefined}
      onFocus={() => context.onSelect(key)}
      title={o.title ?? t.untitled}
      className={cn(
        'outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
        'data-selected:bg-bg-selected',
        className,
      )}
      style={style}
    >
      {children}
    </Link>
  )
}

function Title({ o }: { o: CalendarOccurrence }) {
  return (
    <span
      className={cn(
        'min-w-0 truncate text-fg',
        (o.cancelled || o.removed) && 'text-fg-muted line-through',
      )}
    >
      {o.title || t.untitled}
    </span>
  )
}

function dayHeading(day: DayKey, style: 'long' | 'short' = 'long'): string {
  return new Intl.DateTimeFormat(undefined, {
    weekday: style,
    day: 'numeric',
    month: style,
    timeZone: 'UTC',
  }).format(dateOf(day))
}

function AgendaView({
  days,
  byDay: map,
  context,
}: {
  days: readonly DayKey[]
  byDay: Map<DayKey, CalendarOccurrence[]>
  context: ViewContext
}) {
  const filled = days.filter((d) => (map.get(d)?.length ?? 0) > 0)
  if (filled.length === 0) {
    return (
      <EmptyState icon={<CalendarDaysIcon />} title={t.emptyTitle}>
        <p>{t.emptyLead}</p>
      </EmptyState>
    )
  }
  return (
    <div className="h-full overflow-y-auto">
      {filled.map((day) => (
        <section key={day} aria-label={dayHeading(day)}>
          <h2
            className={cn(
              'sticky top-0 z-10 flex h-8 items-center gap-2 border-b border-border bg-bg-subtle px-4 font-mono text-xs text-fg-subtle',
              day === context.today && 'text-accent',
            )}
          >
            {dayHeading(day)}
            {day === context.today && <span className="font-sans">· {t.today}</span>}
          </h2>
          <ul>
            {(map.get(day) ?? []).map((o) => (
              <li key={occurrenceKey(o)} className="border-b border-border">
                <EventLink
                  o={o}
                  context={context}
                  className="flex items-center gap-3 px-4 py-2 text-sm"
                >
                  <span className="w-28 shrink-0 font-mono text-xs text-fg-subtle tabular-nums">
                    {timeLabel(o, day, context.zone)}
                  </span>
                  <Swatch color={context.colors.get(o.collection_id)} />
                  <span className="flex min-w-0 flex-1 flex-col">
                    <Title o={o} />
                    {o.location && (
                      <span className="truncate text-xs text-fg-subtle">{o.location}</span>
                    )}
                  </span>
                  <Marks o={o} />
                </EventLink>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  )
}

function Chip({ o, context }: { o: CalendarOccurrence; context: ViewContext }) {
  const color = context.colors.get(o.collection_id)
  return (
    <EventLink
      o={o}
      context={context}
      className="flex h-5 items-center gap-1 border-l-2 border-border-strong px-1 text-xs"
      style={color ? { borderLeftColor: color } : undefined}
    >
      {!o.all_day && (
        <span className="shrink-0 font-mono text-[0.625rem] text-fg-subtle tabular-nums">
          {o.floating
            ? o.start_local.slice(11, 16)
            : formatTime(wallClock(o.starts_at, context.zone).minutes)}
        </span>
      )}
      <Title o={o} />
      {o.copies > 1 && (
        <span className="shrink-0 font-mono text-[0.625rem] text-fg-subtle">×{o.copies}</span>
      )}
    </EventLink>
  )
}

function formatTime(minutes: number): string {
  return `${String(Math.floor(minutes / 60)).padStart(2, '0')}:${String(minutes % 60).padStart(2, '0')}`
}

function WeekView({
  days,
  byDay: map,
  context,
}: {
  days: readonly DayKey[]
  byDay: Map<DayKey, CalendarOccurrence[]>
  context: ViewContext
}) {
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const element = scroller.current
    if (element) element.scrollTop = (7 * hourRem * element.scrollHeight) / (24 * hourRem)
  }, [])
  const columns = 'grid-cols-[3.5rem_repeat(7,minmax(0,1fr))]'
  const nowWall = wallClock(context.now, context.zone)
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className={cn('grid shrink-0 border-b border-border', columns)}>
        <span />
        {days.map((day) => (
          <button
            key={day}
            type="button"
            onClick={() => context.onPickDay(day)}
            className={cn(
              'flex h-10 flex-col items-start justify-center border-l border-border px-2 text-left text-xs text-fg-subtle outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
              day === context.today && 'text-accent',
            )}
          >
            <span>
              {new Intl.DateTimeFormat(undefined, { weekday: 'short', timeZone: 'UTC' }).format(
                dateOf(day),
              )}
            </span>
            <span className="font-mono">{Number(day.slice(8))}</span>
          </button>
        ))}
      </div>
      <div className={cn('grid shrink-0 border-b border-border', columns)}>
        <span className="px-1 py-1 text-[0.625rem] text-fg-subtle">{t.allDay}</span>
        {days.map((day) => (
          <ul key={day} className="grid min-h-6 content-start gap-px border-l border-border p-px">
            {(map.get(day) ?? [])
              .filter((o) => o.all_day)
              .map((o) => (
                <li key={occurrenceKey(o)}>
                  <Chip o={o} context={context} />
                </li>
              ))}
          </ul>
        ))}
      </div>
      <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto">
        <div className={cn('relative grid', columns)} style={{ height: `${24 * hourRem}rem` }}>
          <div aria-hidden className="relative">
            {Array.from({ length: 24 }, (_, hour) => (
              <span
                key={hour}
                className="absolute right-2 -translate-y-1/2 font-mono text-[0.625rem] text-fg-subtle"
                style={{ top: `${hour * hourRem}rem` }}
              >
                {hour === 0 ? '' : formatTime(hour * 60)}
              </span>
            ))}
          </div>
          {days.map((day) => {
            const timed = (map.get(day) ?? []).filter((o) => !o.all_day)
            const blocks = timed.map((o) => {
              const { start, end } = dayMinutes(o, day, context.zone)
              return { o, id: occurrenceKey(o), start, end }
            })
            const slots = layoutColumns(blocks)
            return (
              <div key={day} className="relative border-l border-border">
                {Array.from({ length: 24 }, (_, hour) => (
                  <span
                    key={hour}
                    aria-hidden
                    className="absolute inset-x-0 border-t border-border"
                    style={{ top: `${hour * hourRem}rem` }}
                  />
                ))}
                {day === context.today && (
                  <span
                    aria-hidden
                    className="absolute inset-x-0 z-10 border-t border-danger"
                    style={{ top: `${(nowWall.minutes / 60) * hourRem}rem` }}
                  />
                )}
                <ul aria-label={dayHeading(day)}>
                  {blocks.map(({ o, id, start, end }) => {
                    const slot = slots.get(id) ?? { column: 0, columns: 1 }
                    const color = context.colors.get(o.collection_id)
                    return (
                      <li
                        key={id}
                        className="absolute px-px"
                        style={{
                          top: `${(start / 60) * hourRem}rem`,
                          height: `${(Math.max(end - start, 15) / 60) * hourRem}rem`,
                          left: `${(slot.column / slot.columns) * 100}%`,
                          width: `${100 / slot.columns}%`,
                        }}
                      >
                        <EventLink
                          o={o}
                          context={context}
                          className="flex h-full flex-col overflow-hidden border border-l-2 border-border bg-bg-subtle px-1 text-xs"
                          style={color ? { borderLeftColor: color } : undefined}
                        >
                          <Title o={o} />
                          <span className="truncate font-mono text-[0.625rem] text-fg-subtle tabular-nums">
                            {timeLabel(o, day, context.zone)}
                            {o.copies > 1 && ` · ×${o.copies}`}
                          </span>
                        </EventLink>
                      </li>
                    )
                  })}
                </ul>
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}

const maxChips = 3

function MonthView({
  days,
  byDay: map,
  anchor,
  context,
}: {
  days: readonly DayKey[]
  byDay: Map<DayKey, CalendarOccurrence[]>
  anchor: DayKey
  context: ViewContext
}) {
  const weeks = Array.from({ length: Math.ceil(days.length / 7) }, (_, i) =>
    days.slice(i * 7, i * 7 + 7),
  )
  const month = anchor.slice(0, 7)
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="grid shrink-0 grid-cols-7 border-b border-border">
        {(weeks[0] ?? []).map((day) => (
          <span key={day} className="px-2 py-1 text-xs text-fg-subtle">
            {new Intl.DateTimeFormat(undefined, { weekday: 'short', timeZone: 'UTC' }).format(
              dateOf(day),
            )}
          </span>
        ))}
      </div>
      <div
        className="grid min-h-0 flex-1 overflow-y-auto"
        style={{ gridTemplateRows: `repeat(${weeks.length}, minmax(6.5rem, 1fr))` }}
      >
        {weeks.map((week) => (
          <div key={week[0]} className="grid grid-cols-7 border-b border-border">
            {week.map((day) => {
              const list = map.get(day) ?? []
              const outside = day.slice(0, 7) !== month
              return (
                <section
                  key={day}
                  aria-label={dayHeading(day)}
                  className={cn(
                    'flex min-w-0 flex-col gap-px border-l border-border p-px first:border-l-0',
                    outside && 'bg-bg-subtle',
                  )}
                >
                  <button
                    type="button"
                    onClick={() => context.onPickDay(day)}
                    className={cn(
                      'self-start px-1 font-mono text-xs text-fg-subtle outline-none hover:text-fg focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-focus',
                      day === context.today && 'bg-accent text-accent-fg hover:text-accent-fg',
                    )}
                  >
                    {Number(day.slice(8))}
                  </button>
                  <ul className="grid gap-px">
                    {list.slice(0, maxChips).map((o) => (
                      <li key={occurrenceKey(o)}>
                        <Chip o={o} context={context} />
                      </li>
                    ))}
                  </ul>
                  {list.length > maxChips && (
                    <button
                      type="button"
                      onClick={() => context.onPickDay(day)}
                      className="self-start px-1 text-[0.625rem] text-fg-subtle outline-none hover:text-fg focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-focus"
                    >
                      {t.more(list.length - maxChips)}
                    </button>
                  )}
                </section>
              )
            })}
          </div>
        ))}
      </div>
    </div>
  )
}
