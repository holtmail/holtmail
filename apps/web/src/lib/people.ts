import type {
  AddressPerson,
  Collection,
  PersonField,
  PersonLink,
  PersonOverride,
  PersonSummary,
  ValueSource,
} from '@holtmail/api-client'

import { daysBetween } from './format.ts'

interface Named {
  name?: string | null
  email?: string | null
  phone?: string | null
}

export function personLabel(person: Named, fallback: string): string {
  return person.name?.trim() || person.email || person.phone || fallback
}

export function initials(label: string): string {
  const words = label
    .replace(/[<>"@].*$/, '')
    .split(/\s+/)
    .filter((word) => /\p{L}/u.test(word))
  const letters = words.map((word) => /\p{L}/u.exec(word)?.[0] ?? '')
  const picked = letters.length > 1 ? [letters[0], letters.at(-1)] : letters.slice(0, 1)
  return picked.join('').toUpperCase()
}

export function sourceLabel(source: Pick<ValueSource, 'account_name' | 'collection_name'>): string {
  const account = source.account_name?.trim()
  const collection = source.collection_name.trim()
  if (account && collection && account !== collection) return `${account} · ${collection}`
  return account || collection
}

export function seenLabel(seenAt: number, removed: boolean, now: number, locale?: string): string {
  const date = new Date(seenAt)
  if (removed) {
    const when = new Intl.DateTimeFormat(locale, { dateStyle: 'medium' }).format(date)
    return `removed ${when}`
  }
  const days = daysBetween(seenAt, now)
  if (days <= 0) return 'seen today'
  if (days === 1) return 'seen yesterday'
  if (date.getFullYear() === new Date(now).getFullYear()) {
    const when = new Intl.DateTimeFormat(locale, { day: 'numeric', month: 'short' }).format(date)
    return `seen ${when}`
  }
  return `last seen ${date.getFullYear()}`
}

export function addressPeople(people: readonly AddressPerson[]): Map<string, string> {
  return new Map(people.map((p) => [p.email.trim().toLowerCase(), p.person_id]))
}

export function personFor(people: Map<string, string>, email: string): string | undefined {
  return people.get(email.trim().toLowerCase())
}

const fieldNames: Record<string, string> = {
  name: 'Name',
  nickname: 'Nickname',
  email: 'Email',
  phone: 'Phone',
  organization: 'Organization',
  title: 'Title',
  birthday: 'Birthday',
  address: 'Address',
  note: 'Note',
}

export function personFieldLabel(name: string): string {
  return fieldNames[name] ?? name.charAt(0).toUpperCase() + name.slice(1)
}

export function differingFields(fields: readonly PersonField[]): string[] {
  return fields.filter((f) => f.differs).map((f) => personFieldLabel(f.name))
}

export function linkReason(link: Pick<PersonLink, 'kind' | 'value'>): string {
  if (link.kind === 'hand') return 'joined by hand'
  return link.kind === 'phone' ? `same phone ${link.value}` : `same email ${link.value}`
}

/** Whether moving `chosen` of a person's `total` cards out leaves both sides with a card. */
export function canSplit(total: number, chosen: number): boolean {
  return chosen > 0 && chosen < total
}

/** Toggles `id` in a choice of cards, keeping their order of choosing. */
export function toggleChoice(chosen: readonly string[], id: string): string[] {
  return chosen.includes(id) ? chosen.filter((c) => c !== id) : [...chosen, id]
}

/** People a person can be joined with: everyone listed except the person, without repeats. */
export function joinCandidates(
  people: readonly PersonSummary[],
  selfId: string,
  limit = 20,
): PersonSummary[] {
  const seen = new Set<string>([selfId])
  const out: PersonSummary[] = []
  for (const person of people) {
    if (seen.has(person.id)) continue
    seen.add(person.id)
    out.push(person)
    if (out.length >= limit) break
  }
  return out
}

export interface OverrideNote {
  /** `joined` or `split`. */
  kind: 'joined' | 'split'
  /** The override to undo. */
  id: string
  createdAt: number
  /** The other people it names: the person split off from this one, or the other side. */
  others: string[]
}

/** What the person page says about splits and joins made by hand that touch the person. */
export function overrideNotes(
  overrides: readonly PersonOverride[],
  personId: string,
): OverrideNote[] {
  return overrides.map((o) => ({
    kind: o.kind === 'join' ? 'joined' : 'split',
    id: o.id,
    createdAt: o.created_at,
    others: o.people.filter((p) => p !== personId),
  }))
}

export interface YearBar {
  year: number
  messages: number
  share: number
}

export function yearBars(years: readonly { year: number; messages: number }[]): YearBar[] {
  if (years.length === 0) return []
  const first = Math.min(...years.map((y) => y.year))
  const last = Math.max(...years.map((y) => y.year))
  const most = Math.max(1, ...years.map((y) => y.messages))
  const counts = new Map(years.map((y) => [y.year, y.messages]))
  const out: YearBar[] = []
  for (let year = first; year <= last; year += 1) {
    const messages = counts.get(year) ?? 0
    out.push({ year, messages, share: messages / most })
  }
  return out
}

export interface SourceOption {
  value: string
  label: string
}

export function sourceOptions(
  collections: readonly Collection[],
  labels: { all: string; contacts: string; mail: string },
): SourceOption[] {
  const books = collections
    .filter((c) => c.kind === 'contacts' && c.items + c.removed_items > 0)
    .map((c) => ({
      value: c.id,
      label: sourceLabel({ account_name: c.account_name, collection_name: c.name }),
    }))
  return [
    { value: 'all', label: labels.all },
    { value: 'contacts', label: labels.contacts },
    { value: 'mail', label: labels.mail },
    ...books,
  ]
}

export interface ContactView {
  name?: string
  emails: string[]
  phones: string[]
  organization?: string
  title?: string
  birthday?: string
  note?: string
}

function field(value: unknown, key: string): unknown {
  if (typeof value !== 'object' || value === null) return undefined
  return new Map(Object.entries(value)).get(key)
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined
}

function list(value: unknown, key: string): string[] {
  return Array.isArray(value)
    ? value.flatMap((entry: unknown) => text(field(entry, key)) ?? [])
    : []
}

export function readContact(data: unknown): ContactView | undefined {
  if (field(data, 'type') !== 'contact') return undefined
  const parts = field(data, 'name')
  const fromParts = ['prefix', 'given', 'additional', 'family', 'suffix']
    .flatMap((key) => text(field(parts, key)) ?? [])
    .join(' ')
  return {
    name: text(field(data, 'full_name')) ?? text(fromParts),
    emails: list(field(data, 'emails'), 'address'),
    phones: list(field(data, 'phones'), 'number'),
    organization: text(field(data, 'organization')),
    title: text(field(data, 'title')),
    birthday: text(field(data, 'birthday')),
    note: text(field(data, 'note')),
  }
}
