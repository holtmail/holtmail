import type {
  Collection,
  ContactForm,
  EventForm,
  Labeled,
  LocalEdit,
  PersonForm,
  PimChange,
  PimResolution,
  PostalAddress,
  WriteMode,
} from '@holtmail/api-client'

export type EditKind = 'event' | 'contact'

/** Address books or calendars the user may create items in now, by name. */
export function editableCollections(
  collections: readonly Collection[] | undefined,
  kind: EditKind,
): Collection[] {
  const wanted = kind === 'contact' ? 'contacts' : 'calendar'
  return (collections ?? [])
    .filter((c) => c.kind === wanted && c.editable && c.removed_at == null)
    .toSorted((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id))
}

/** Whether a person's card can be edited: their primary card's address book is editable, or,
 * without one, an address book a new card would go to is. */
export function cardEditable(
  detail: {
    primary_card?: string | null
    cards: readonly { item_id: string; collection_id: string }[]
  },
  collections: readonly Collection[] | undefined,
): boolean {
  if (detail.primary_card) {
    const card = detail.cards.find((c) => c.item_id === detail.primary_card)
    return Boolean(card && collections?.find((c) => c.id === card.collection_id)?.editable)
  }
  return editableCollections(collections, 'contact').length > 0
}

export type MarkState = 'held' | 'sending' | 'conflict' | 'rejected'

export function markState(edit: Pick<LocalEdit, 'state'>): MarkState | undefined {
  const state = edit.state
  return state === 'held' || state === 'sending' || state === 'conflict' || state === 'rejected'
    ? state
    : undefined
}

export interface MarkStrings {
  held: (account: string) => string
  sending: string
  conflict: (account: string) => string
  rejected: (account: string) => string
  account: string
}

/** The words of a Local edit mark: "not sent to <account>", "sending", "changed in <account>
 * too" or "<account> refused". */
export function markText(
  edit: Pick<LocalEdit, 'state' | 'account_name'>,
  s: MarkStrings,
): string | undefined {
  const account = edit.account_name?.trim() || s.account
  switch (markState(edit)) {
    case 'held':
      return s.held(account)
    case 'sending':
      return s.sending
    case 'conflict':
      return s.conflict(account)
    case 'rejected':
      return s.rejected(account)
    default:
      return undefined
  }
}

/** What a person may do with an open change: decide a conflict (keeping their version needs an
 * item to put it on), retry or drop a refusal, drop a held change; nothing while it is sent. */
export function changeActions(change: Pick<PimChange, 'state' | 'item_id'>): PimResolution[] {
  switch (change.state) {
    case 'conflict':
      return change.item_id ? ['keep_mine', 'take_theirs', 'discard'] : ['discard']
    case 'rejected':
      return ['retry', 'discard']
    case 'held':
      return ['discard']
    default:
      return []
  }
}

export const needsDecision = (change: Pick<PimChange, 'state'>): boolean =>
  change.state === 'conflict' || change.state === 'rejected'

export const decisionCount = (changes: readonly Pick<PimChange, 'state'>[] | undefined): number =>
  (changes ?? []).filter(needsDecision).length

export interface ChangeGroup {
  collectionId: string
  collection?: Collection
  changes: PimChange[]
}

/** Open changes by collection: collections by name (unknown ones last), changes oldest first. */
export function groupByCollection(
  changes: readonly PimChange[],
  collections: readonly Collection[],
): ChangeGroup[] {
  const groups = new Map<string, ChangeGroup>()
  for (const change of changes) {
    const group = groups.get(change.collection_id) ?? {
      collectionId: change.collection_id,
      collection: collections.find((c) => c.id === change.collection_id),
      changes: [],
    }
    group.changes.push(change)
    groups.set(change.collection_id, group)
  }
  for (const group of groups.values()) {
    group.changes.sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id))
  }
  return [...groups.values()].toSorted((a, b) => {
    if (!a.collection || !b.collection) return a.collection ? -1 : b.collection ? 1 : 0
    return a.collection.name.localeCompare(b.collection.name)
  })
}

export const writeModes: WriteMode[] = ['read_only', 'held', 'write_through']

export function isWriteMode(value: string): value is WriteMode {
  return value === 'read_only' || value === 'held' || value === 'write_through'
}

/** The modes the picker offers: those the server says the user may set now (`allowed`, from
 * the collection's write mode), else only its current one; only `read_only` without an
 * account. */
export function modeChoices(
  collection: Pick<Collection, 'account_id' | 'write_mode'>,
  allowed?: readonly string[],
): WriteMode[] {
  if (!collection.account_id) return ['read_only']
  const current = isWriteMode(collection.write_mode) ? [collection.write_mode] : []
  if (!allowed) return current
  return writeModes.filter((m) => allowed.includes(m) || current.includes(m))
}

/** Turning on sending asks first: every open change goes to the account. */
export const asksToConfirm = (from: string, to: WriteMode): boolean =>
  to === 'write_through' && from !== 'write_through'

export const openCounts = (changes: readonly PimChange[]): Map<string, number> => {
  const counts = new Map<string, number>()
  for (const change of changes) {
    counts.set(change.collection_id, (counts.get(change.collection_id) ?? 0) + 1)
  }
  return counts
}

const pad = (n: number) => String(n).padStart(2, '0')

function civil(date: string): Date | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date)
  if (!match) return undefined
  const d = new Date(Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3])))
  return Number.isNaN(d.getTime()) ? undefined : d
}

const dayText = (d: Date) =>
  `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`

/** `date` moved by `days` on the calendar, ignoring zones. */
export function addDays(date: string, days: number): string {
  const d = civil(date)
  if (!d) return date
  d.setUTCDate(d.getUTCDate() + days)
  return dayText(d)
}

/** A wall-clock date and time moved by `minutes`, ignoring zones. */
export function addMinutes(date: string, time: string, minutes: number): [string, string] {
  const d = civil(date)
  const t = /^(\d{2}):(\d{2})/.exec(time)
  if (!d || !t) return [date, time]
  d.setUTCMinutes(Number(t[1]) * 60 + Number(t[2]) + minutes)
  return [dayText(d), `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`]
}

function split(value: string | null | undefined): [string, string] {
  if (!value) return ['', '']
  const [date = '', time = ''] = value.split('T')
  return [date, time.slice(0, 5)]
}

export type Repeat = 'none' | 'daily' | 'weekly' | 'monthly' | 'yearly' | 'custom'

export const repeats: Repeat[] = ['none', 'daily', 'weekly', 'monthly', 'yearly', 'custom']

export function isRepeat(value: string): value is Repeat {
  return repeats.some((r) => r === value)
}

const presets = ['daily', 'weekly', 'monthly', 'yearly'] as const

const presetRules: Record<(typeof presets)[number], string> = {
  daily: 'FREQ=DAILY',
  weekly: 'FREQ=WEEKLY',
  monthly: 'FREQ=MONTHLY',
  yearly: 'FREQ=YEARLY',
}

export function repeatOf(rule: string | null | undefined): Repeat {
  const value = rule?.trim().toUpperCase()
  if (!value) return 'none'
  return presets.find((p) => presetRules[p] === value) ?? 'custom'
}

export const alarmChoices = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080]

export interface EventDraft {
  uid: string | null
  title: string
  allDay: boolean
  startDate: string
  startTime: string
  endDate: string
  endTime: string
  timeZone: string
  repeat: Repeat
  rule: string
  location: string
  description: string
  organizer: PersonForm | null
  attendees: PersonForm[]
  alarms: number[]
}

/** The viewer's calendar date of `now`. */
export const localDay = (now: Date): string =>
  `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`

/** A new event on `date`: at the next full hour when `date` is today, else 09:00; one hour long,
 * in `zone`, reminded 15 minutes before. */
export function newEventDraft(date: string, zone: string, now: Date): EventDraft {
  const today = localDay(now)
  const [startDate, startTime] =
    date === today ? addMinutes(today, `${pad(now.getHours())}:00`, 60) : [date, '09:00']
  const [endDate, endTime] = addMinutes(startDate, startTime, 60)
  return {
    uid: null,
    title: '',
    allDay: false,
    startDate,
    startTime,
    endDate,
    endTime,
    timeZone: zone,
    repeat: 'none',
    rule: '',
    location: '',
    description: '',
    organizer: null,
    attendees: [],
    alarms: [15],
  }
}

/** The form an edit dialog starts from. An all-day end is shown inclusive; a missing end as one
 * day or one hour after the start. */
export function eventDraft(form: EventForm): EventDraft {
  const [startDate, startTime] = split(form.start)
  let [endDate, endTime] = split(form.end)
  if (form.all_day) {
    endDate = endDate ? addDays(endDate, -1) : startDate
    if (endDate < startDate) endDate = startDate
  } else if (!endDate) {
    ;[endDate, endTime] = addMinutes(startDate, startTime || '00:00', 60)
  }
  return {
    uid: form.uid,
    title: form.title,
    allDay: form.all_day,
    startDate,
    startTime: form.all_day ? '09:00' : startTime,
    endDate,
    endTime: form.all_day ? '10:00' : endTime,
    timeZone: form.all_day ? '' : (form.time_zone ?? ''),
    repeat: repeatOf(form.recurrence),
    rule: form.recurrence ?? '',
    location: form.location ?? '',
    description: form.description ?? '',
    organizer: form.organizer,
    attendees: form.attendees.map((a) => ({ ...a })),
    alarms: form.alarms.map((a) => a.minutes_before),
  }
}

export type EventProblem = 'start' | 'time' | 'endBefore' | 'email' | 'rule'

export interface Invalid<P> {
  problem: P
  value?: string
}

const looksLikeAddress = (value: string) => /^[^\s@]+@[^\s@]+$/.test(value.trim())

const blank = (value: string | null | undefined) => {
  const trimmed = value?.trim()
  return trimmed ? trimmed : null
}

/** The request an event draft becomes, or what is wrong with it. `original` keeps the times
 * exactly as served when the draft did not change them. */
export function eventRequest(
  draft: EventDraft,
  original?: EventForm,
): { form: EventForm } | Invalid<EventProblem> {
  if (!civil(draft.startDate)) return { problem: 'start' }
  if (!draft.allDay && !/^\d{2}:\d{2}$/.test(draft.startTime)) return { problem: 'time' }
  let start: string
  let end: string | null
  if (draft.allDay) {
    const last = civil(draft.endDate) ? draft.endDate : draft.startDate
    if (last < draft.startDate) return { problem: 'endBefore' }
    start = draft.startDate
    end = addDays(last, 1)
  } else {
    const endDate = civil(draft.endDate) ? draft.endDate : draft.startDate
    const endTime = /^\d{2}:\d{2}$/.test(draft.endTime) ? draft.endTime : draft.startTime
    start = `${draft.startDate}T${draft.startTime}`
    end = `${endDate}T${endTime}`
    if (end < start) return { problem: 'endBefore' }
  }
  if (original && original.all_day === draft.allDay) {
    const before = eventDraft(original)
    const sameStart =
      before.startDate === draft.startDate && (draft.allDay || before.startTime === draft.startTime)
    const sameEnd =
      before.endDate === draft.endDate && (draft.allDay || before.endTime === draft.endTime)
    if (sameStart) start = original.start
    if (sameStart && sameEnd) end = original.end
  }
  for (const attendee of draft.attendees) {
    if (!looksLikeAddress(attendee.email)) return { problem: 'email', value: attendee.email }
  }
  let recurrence: string | null = null
  if (draft.repeat === 'custom') {
    recurrence = blank(draft.rule)
    if (!recurrence) return { problem: 'rule' }
  } else if (draft.repeat !== 'none') {
    recurrence =
      original && repeatOf(original.recurrence) === draft.repeat
        ? original.recurrence
        : presetRules[draft.repeat]
  }
  return {
    form: {
      uid: draft.uid,
      title: draft.title.trim(),
      all_day: draft.allDay,
      start,
      end,
      time_zone: draft.allDay ? null : blank(draft.timeZone),
      recurrence,
      location: blank(draft.location),
      description: blank(draft.description),
      organizer: draft.organizer,
      attendees: draft.attendees.map((a) => ({
        email: a.email.trim(),
        name: blank(a.name),
        partstat: a.partstat,
      })),
      alarms: [...new Set(draft.alarms)].map((minutes) => ({ minutes_before: minutes })),
    },
  }
}

/** Moving the start keeps the length of the event. */
export function withStart(draft: EventDraft, date: string, time: string): EventDraft {
  if (draft.allDay) {
    const length = daysBetween(draft.startDate, draft.endDate)
    return {
      ...draft,
      startDate: date,
      endDate: civil(date) ? addDays(date, Math.max(0, length)) : draft.endDate,
    }
  }
  const minutes = minutesBetween(draft.startDate, draft.startTime, draft.endDate, draft.endTime)
  const [endDate, endTime] =
    minutes === undefined ? [draft.endDate, draft.endTime] : addMinutes(date, time, minutes)
  return { ...draft, startDate: date, startTime: time, endDate, endTime }
}

function daysBetween(from: string, to: string): number {
  const a = civil(from)
  const b = civil(to)
  return a && b ? Math.round((b.getTime() - a.getTime()) / 86_400_000) : 0
}

function minutesBetween(
  fromDate: string,
  fromTime: string,
  toDate: string,
  toTime: string,
): number | undefined {
  const a = civil(fromDate)
  const b = civil(toDate)
  const ta = /^(\d{2}):(\d{2})$/.exec(fromTime)
  const tb = /^(\d{2}):(\d{2})$/.exec(toTime)
  if (!a || !b || !ta || !tb) return undefined
  const minutes =
    (b.getTime() - a.getTime()) / 60_000 +
    Number(tb[1]) * 60 +
    Number(tb[2]) -
    Number(ta[1]) * 60 -
    Number(ta[2])
  return minutes >= 0 ? minutes : undefined
}

export interface LabeledRow {
  value: string
  label: string
  extra: string[]
}

export interface AddressRow {
  label: string
  extra: string[]
  street: string
  extended: string
  poBox: string
  locality: string
  region: string
  postalCode: string
  country: string
}

export interface ContactDraft {
  uid: string | null
  fullName: string
  given: string
  family: string
  additional: string
  prefix: string
  suffix: string
  organization: string
  title: string
  emails: LabeledRow[]
  phones: LabeledRow[]
  addresses: AddressRow[]
  notes: string
}

export const emailLabels = ['', 'home', 'work']
export const phoneLabels = ['', 'cell', 'home', 'work']
export const addressLabels = ['', 'home', 'work']

function splitKinds(kinds: readonly string[], labels: readonly string[]): [string, string[]] {
  const label = kinds.find((k) => k !== '' && labels.includes(k)) ?? ''
  return [label, kinds.filter((k) => k !== label)]
}

const joinKinds = (label: string, extra: readonly string[]) =>
  label ? [label, ...extra.filter((k) => k !== label)] : [...extra]

function labeledRow(l: Labeled, labels: readonly string[]): LabeledRow {
  const [label, extra] = splitKinds(l.kinds, labels)
  return { value: l.value, label, extra }
}

export const emptyLabeled = (label = ''): LabeledRow => ({ value: '', label, extra: [] })

export const emptyAddress = (): AddressRow => ({
  label: '',
  extra: [],
  street: '',
  extended: '',
  poBox: '',
  locality: '',
  region: '',
  postalCode: '',
  country: '',
})

/** The shown name made from the parts, as vCard readers order them. */
export function composedName(
  d: Pick<ContactDraft, 'prefix' | 'given' | 'additional' | 'family' | 'suffix'>,
): string {
  return [d.prefix, d.given, d.additional, d.family, d.suffix]
    .map((p) => p.trim())
    .filter(Boolean)
    .join(' ')
}

export function contactDraft(form?: Partial<ContactForm>): ContactDraft {
  const name = form?.name
  return {
    uid: form?.uid ?? null,
    fullName: form?.full_name ?? '',
    given: name?.given ?? '',
    family: name?.family ?? '',
    additional: name?.additional ?? '',
    prefix: name?.prefix ?? '',
    suffix: name?.suffix ?? '',
    organization: form?.organization ?? '',
    title: form?.title ?? '',
    emails: (form?.emails ?? []).map((e) => labeledRow(e, emailLabels)),
    phones: (form?.phones ?? []).map((p) => labeledRow(p, phoneLabels)),
    addresses: (form?.addresses ?? []).map((a) => {
      const [label, extra] = splitKinds(a.kinds, addressLabels)
      return {
        label,
        extra,
        street: a.street ?? '',
        extended: a.extended ?? '',
        poBox: a.po_box ?? '',
        locality: a.locality ?? '',
        region: a.region ?? '',
        postalCode: a.postal_code ?? '',
        country: a.country ?? '',
      }
    }),
    notes: form?.notes ?? '',
  }
}

type NamePart = 'given' | 'family' | 'additional' | 'prefix' | 'suffix'

/** A changed name part; the shown name follows the parts while it was made from them. */
export function withNamePart(draft: ContactDraft, part: NamePart, value: string): ContactDraft {
  const follows = draft.fullName.trim() === '' || draft.fullName.trim() === composedName(draft)
  const next = { ...draft, [part]: value }
  return follows ? { ...next, fullName: composedName(next) } : next
}

export type ContactProblem = 'empty' | 'email'

/** The request a contact draft becomes, or what is wrong with it. Empty rows are left out. */
export function contactRequest(
  draft: ContactDraft,
): { form: ContactForm } | Invalid<ContactProblem> {
  const emails = draft.emails.filter((e) => e.value.trim() !== '')
  for (const email of emails) {
    if (!looksLikeAddress(email.value)) return { problem: 'email', value: email.value.trim() }
  }
  const phones = draft.phones.filter((p) => p.value.trim() !== '')
  const addresses: PostalAddress[] = draft.addresses
    .map((a) => ({
      kinds: joinKinds(a.label, a.extra),
      street: blank(a.street),
      extended: blank(a.extended),
      po_box: blank(a.poBox),
      locality: blank(a.locality),
      region: blank(a.region),
      postal_code: blank(a.postalCode),
      country: blank(a.country),
    }))
    .filter((a) =>
      [a.street, a.extended, a.po_box, a.locality, a.region, a.postal_code, a.country].some(
        Boolean,
      ),
    )
  const fullName = blank(draft.fullName) ?? blank(composedName(draft))
  if (!fullName && !blank(draft.organization) && emails.length === 0) return { problem: 'empty' }
  return {
    form: {
      uid: draft.uid,
      full_name: fullName,
      name: {
        given: blank(draft.given),
        family: blank(draft.family),
        additional: blank(draft.additional),
        prefix: blank(draft.prefix),
        suffix: blank(draft.suffix),
      },
      emails: emails.map((e) => ({ value: e.value.trim(), kinds: joinKinds(e.label, e.extra) })),
      phones: phones.map((p) => ({ value: p.value.trim(), kinds: joinKinds(p.label, p.extra) })),
      addresses,
      organization: blank(draft.organization),
      title: blank(draft.title),
      notes: blank(draft.notes),
    },
  }
}

/** A contact draft for a message's sender: their name as the shown name, split into first and
 * last name at the last space, and their address. */
export function senderDraft(name: string | null | undefined, email: string): ContactDraft {
  const shown = name?.trim() ?? ''
  const cut = shown.lastIndexOf(' ')
  return {
    ...contactDraft(),
    fullName: shown,
    given: cut > 0 ? shown.slice(0, cut) : shown,
    family: cut > 0 ? shown.slice(cut + 1) : '',
    emails: [{ value: email, label: '', extra: [] }],
  }
}

/** The sender a message's "Add sender to contacts" would add: its first shown sender, unless
 * that address already belongs to a person with a contact card. A person known from mail alone
 * can still be added. */
export function newSender(
  from: readonly { email: string; name?: string | null }[],
  people: readonly { email: string; has_card: boolean }[],
): { email: string; name?: string | null } | undefined {
  const sender = from[0]
  if (!sender?.email.includes('@')) return undefined
  const email = sender.email.toLowerCase()
  return people.some((p) => p.has_card && p.email.toLowerCase() === email) ? undefined : sender
}

/** Minimal iCalendar or vCard text to start raw editing of a new item from. */
export function rawTemplate(kind: EditKind, uid: string, now: Date): string {
  const stamp = now.toISOString().replaceAll(/[-:]/g, '').replace(/\.\d+/, '')
  if (kind === 'contact') {
    return ['BEGIN:VCARD', 'VERSION:3.0', `UID:${uid}`, 'FN:', 'N:;;;;', 'END:VCARD', ''].join(
      '\r\n',
    )
  }
  const day = stamp.slice(0, 8)
  return [
    'BEGIN:VCALENDAR',
    'VERSION:2.0',
    'PRODID:-//Holtmail//EN',
    'BEGIN:VEVENT',
    `UID:${uid}`,
    `DTSTAMP:${stamp}`,
    `DTSTART;VALUE=DATE:${day}`,
    'SUMMARY:',
    'END:VEVENT',
    'END:VCALENDAR',
    '',
  ].join('\r\n')
}

/** Time zones the browser knows, the viewer's first. */
export function zoneChoices(viewer: string): string[] {
  const supported =
    typeof Intl.supportedValuesOf === 'function' ? Intl.supportedValuesOf('timeZone') : []
  return [viewer, 'UTC', ...supported.filter((z) => z !== viewer && z !== 'UTC')]
}
