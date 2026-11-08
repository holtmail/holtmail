import type {
  EventPerson,
  Invitation,
  Organizing,
  Rsvp,
  RsvpOccurrence,
  RsvpOptions,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'
import { aliasProviderName } from './aliases.ts'

type RsvpStrings = typeof strings.edits.rsvp

const fallbackAccount = () => strings.edits.mark.account

/** Whether the card of `invitation` can offer an answer: an invitation or event that is not
 * cancelled. Replies, cancellations and proposals have nothing to answer. */
export function answerable(invitation: Invitation): boolean {
  if (invitation.cancelled) return false
  return !invitation.method || invitation.method === 'request' || invitation.method === 'publish'
}

/** How the answer would leave, in one sentence; `undefined` when it would not be allowed. */
export function routeNote(
  options: RsvpOptions,
  s: RsvpStrings = strings.edits.rsvp,
): string | undefined {
  if (!options.allowed) return undefined
  const account = options.account_name ?? fallbackAccount()
  switch (options.route) {
    case 'server':
      return s.route.server(account)
    case 'quiet':
      return s.route.quiet
    case 'imip': {
      if (options.provider && options.attendee) {
        const service = aliasProviderName(options.provider)
        return options.from && options.from !== options.attendee
          ? s.route.alias(service, options.attendee, options.from)
          : s.route.aliasOnly(service, options.attendee)
      }
      return options.from ? s.route.imip(options.from) : undefined
    }
    default:
      return undefined
  }
}

/** Whether the answer stays in Holtmail for now: the copy is held, or already holds one. */
export function isHeld(options: RsvpOptions): boolean {
  return (
    options.copy === 'held' ||
    (options.local_edit?.class === 'reply' && options.local_edit.state === 'held')
  )
}

/** What happened to an answer once the undo window is over. */
export function doneNote(
  result: Rsvp,
  options: RsvpOptions | undefined,
  s: RsvpStrings = strings.edits.rsvp,
): string {
  switch (result.route) {
    case 'server':
      return s.done.server(options?.account_name ?? fallbackAccount())
    case 'quiet':
      return s.done.quiet
    default:
      return s.done.sent
  }
}

/** The occurrence to answer first: the one the calendar was opened on, else the next one not
 * over yet, else the last. */
export function defaultOccurrence(
  occurrences: readonly RsvpOccurrence[],
  key: string | undefined,
  now: number,
): RsvpOccurrence | undefined {
  return (
    occurrences.find((o) => o.key === key) ??
    occurrences.find((o) => o.ends_at > now) ??
    occurrences.at(-1)
  )
}

/** Whether the page opened on `key` can answer just that occurrence: the capped list has it. */
export const listsOccurrence = (
  occurrences: readonly RsvpOccurrence[],
  key: string | undefined,
): boolean => key !== undefined && occurrences.some((o) => o.key === key)

/** Who sends the invitations of an event being edited, in one sentence. */
export function organizingNote(
  organizing: Organizing,
  s: RsvpStrings = strings.edits.rsvp,
): string {
  const account = organizing.account_name ?? fallbackAccount()
  switch (organizing.route) {
    case 'server':
      return s.invitations.server(account)
    case 'deferred':
      return s.invitations.deferred(account)
    case 'imip':
      return organizing.allowed
        ? s.invitations.imip(organizing.from ?? '')
        : s.invitations.denied(organizing.from ?? '')
    case 'unknown':
      return s.invitations.unknown(account)
    default:
      return s.invitations.none
  }
}

/** Whether saving mails invitations that the user can choose to leave out. */
export const canChooseInvitations = (organizing: Organizing | undefined): boolean =>
  organizing !== undefined &&
  ((organizing.route === 'imip' && organizing.allowed) || organizing.route === 'unknown')

/** `false` asks the server to save without mailing; `null` leaves the choice to the route. */
export function invitationsFlag(organizing: Organizing | undefined, send: boolean): boolean | null {
  if (organizing?.route === 'unknown') return send ? null : false
  if (organizing?.route !== 'imip') return null
  return organizing.allowed && send ? null : false
}

/** Whether a raw event names attendees. */
export const rawHasAttendees = (raw: string): boolean => /^ATTENDEE[;:]/im.test(raw)

/** Whether a save of an event could tell anyone: it has invitees, or had some to tell they are
 * off the list. */
export const reachesPeople = (
  original: readonly { email?: string | null }[] | undefined,
  now: readonly { email: string }[],
): boolean => (original?.length ?? 0) > 0 || now.some((a) => a.email.trim() !== '')

export interface Tally {
  accepted: number
  tentative: number
  declined: number
  none: number
}

/** Attendees by answer; anything that is no answer counts as not answered. */
export function tally(attendees: readonly Pick<EventPerson, 'status'>[]): Tally {
  const out: Tally = { accepted: 0, tentative: 0, declined: 0, none: 0 }
  for (const { status } of attendees) {
    if (status === 'accepted' || status === 'tentative' || status === 'declined') out[status] += 1
    else out.none += 1
  }
  return out
}
