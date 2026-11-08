import { describe, expect, it } from 'bun:test'

import type {
  Invitation,
  Organizing,
  Rsvp,
  RsvpOccurrence,
  RsvpOptions,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'
import {
  answerable,
  canChooseInvitations,
  defaultOccurrence,
  doneNote,
  invitationsFlag,
  isHeld,
  listsOccurrence,
  organizingNote,
  rawHasAttendees,
  reachesPeople,
  routeNote,
  tally,
} from './rsvp.ts'

const s = strings.edits.rsvp

const options = (over: Partial<RsvpOptions>): RsvpOptions => ({
  allowed: true,
  copy: 'none',
  recurring: false,
  occurrences: [],
  ...over,
})

const invitation = (over: Partial<Invitation>): Invitation => ({
  item_id: 'i',
  version: 1,
  all_day: false,
  recurring: false,
  cancelled: false,
  version_count: 1,
  updates_after: 0,
  ...over,
})

const occurrence = (key: string, starts_at: number): RsvpOccurrence => ({
  key,
  starts_at,
  ends_at: starts_at + 10,
  recurrence_id: key,
  start_local: key,
  all_day: false,
})

describe('answerable', () => {
  it('offers an answer on invitations and events, not on the rest', () => {
    expect(answerable(invitation({ method: 'request' }))).toBe(true)
    expect(answerable(invitation({ method: 'publish' }))).toBe(true)
    expect(answerable(invitation({}))).toBe(true)
    expect(answerable(invitation({ method: 'reply' }))).toBe(false)
    expect(answerable(invitation({ method: 'cancel' }))).toBe(false)
    expect(answerable(invitation({ method: 'request', cancelled: true }))).toBe(false)
  })
})

describe('routeNote', () => {
  it('names the address an email answer leaves from', () => {
    expect(routeNote(options({ route: 'imip', from: 'ada@example.com' }))).toBe(
      s.route.imip('ada@example.com'),
    )
  })

  it('names the alias service and the alias', () => {
    expect(
      routeNote(
        options({
          route: 'imip',
          from: 'ada@example.com',
          attendee: 'walk.ab@slmail.example.org',
          provider: 'simplelogin',
        }),
      ),
    ).toBe(s.route.alias('SimpleLogin', 'walk.ab@slmail.example.org', 'ada@example.com'))
  })

  it('does not repeat an alias that is also the sender', () => {
    expect(
      routeNote(
        options({
          route: 'imip',
          from: 'walk.ab@slmail.example.org',
          attendee: 'walk.ab@slmail.example.org',
          provider: 'simplelogin',
        }),
      ),
    ).toBe(s.route.aliasOnly('SimpleLogin', 'walk.ab@slmail.example.org'))
  })

  it('names the account whose server tells the organizer', () => {
    expect(routeNote(options({ route: 'server', account_name: 'Fastmail' }))).toBe(
      s.route.server('Fastmail'),
    )
  })

  it('says nobody is told when the organizer asked for no answers', () => {
    expect(routeNote(options({ route: 'quiet' }))).toBe(s.route.quiet)
  })

  it('stays silent when answering is not allowed', () => {
    expect(routeNote(options({ allowed: false, route: 'imip', from: 'a@example.com' }))).toBe(
      undefined,
    )
  })
})

describe('isHeld', () => {
  const edit = { change_id: 'c', op: 'update', write_mode: 'held', created_at: 1 }

  it('holds when the copy waits or already holds an answer', () => {
    expect(isHeld(options({ copy: 'held' }))).toBe(true)
    expect(
      isHeld(options({ copy: 'put', local_edit: { ...edit, state: 'held', class: 'reply' } })),
    ).toBe(true)
  })

  it('does not count other edits or an answer on its way', () => {
    expect(isHeld(options({ copy: 'respond' }))).toBe(false)
    expect(
      isHeld(options({ copy: 'put', local_edit: { ...edit, state: 'held', class: 'edit' } })),
    ).toBe(false)
    expect(
      isHeld(options({ copy: 'put', local_edit: { ...edit, state: 'sending', class: 'reply' } })),
    ).toBe(false)
  })
})

const rsvp = (route: string): Rsvp => ({ route, copy: 'none' })

describe('doneNote', () => {
  it('says what became of the answer', () => {
    expect(doneNote(rsvp('imip'), undefined)).toBe(s.done.sent)
    expect(doneNote(rsvp('server'), options({ account_name: 'Fastmail' }))).toBe(
      s.done.server('Fastmail'),
    )
    expect(doneNote(rsvp('quiet'), undefined)).toBe(s.done.quiet)
  })
})

describe('defaultOccurrence', () => {
  const list = [occurrence('a', 100), occurrence('b', 200), occurrence('c', 300)]

  it('prefers the occurrence the calendar opened on', () => {
    expect(defaultOccurrence(list, 'a', 250)?.key).toBe('a')
  })

  it('knows when the capped list holds the opened occurrence', () => {
    expect(listsOccurrence(list, 'a')).toBe(true)
    expect(listsOccurrence(list, 'gone')).toBe(false)
    expect(listsOccurrence(list, undefined)).toBe(false)
  })

  it('counts an occurrence still going on as the next', () => {
    expect(defaultOccurrence(list, undefined, 105)?.key).toBe('a')
  })

  it('takes the next to start, else the last', () => {
    expect(defaultOccurrence(list, undefined, 150)?.key).toBe('b')
    expect(defaultOccurrence(list, 'gone', 400)?.key).toBe('c')
    expect(defaultOccurrence([], undefined, 0)).toBe(undefined)
  })
})

describe('organizing', () => {
  const imip: Organizing = { route: 'imip', from: 'ada@example.com', allowed: true }
  const denied: Organizing = { ...imip, allowed: false }
  const server: Organizing = { route: 'server', account_name: 'Fastmail', allowed: true }
  const deferred: Organizing = { route: 'deferred', account_name: 'Fastmail', allowed: true }
  const none: Organizing = { route: 'none', allowed: true }
  const unknown: Organizing = { route: 'unknown', account_name: 'Fastmail', allowed: false }

  it('says who sends the invitations', () => {
    expect(organizingNote(imip)).toBe(s.invitations.imip('ada@example.com'))
    expect(organizingNote(denied)).toBe(s.invitations.denied('ada@example.com'))
    expect(organizingNote(server)).toBe(s.invitations.server('Fastmail'))
    expect(organizingNote(deferred)).toBe(s.invitations.deferred('Fastmail'))
    expect(organizingNote(none)).toBe(s.invitations.none)
    expect(organizingNote(unknown)).toBe(s.invitations.unknown('Fastmail'))
  })

  it('offers the choice only for invitations Holtmail mails', () => {
    expect(canChooseInvitations(imip)).toBe(true)
    expect(canChooseInvitations(denied)).toBe(false)
    expect(canChooseInvitations(server)).toBe(false)
    expect(canChooseInvitations(undefined)).toBe(false)
    expect(canChooseInvitations(unknown)).toBe(true)
  })

  it('saves without invitations when told to or when mail cannot be sent', () => {
    expect(invitationsFlag(imip, true)).toBe(null)
    expect(invitationsFlag(imip, false)).toBe(false)
    expect(invitationsFlag(denied, true)).toBe(false)
    expect(invitationsFlag(server, true)).toBe(null)
    expect(invitationsFlag(undefined, true)).toBe(null)
    expect(invitationsFlag(unknown, true)).toBe(null)
    expect(invitationsFlag(unknown, false)).toBe(false)
  })

  it('sees who a save could reach', () => {
    expect(reachesPeople([{ email: 'bob@example.org' }], [])).toBe(true)
    expect(reachesPeople([], [{ email: ' ' }])).toBe(false)
    expect(reachesPeople(undefined, [{ email: 'bob@example.org' }])).toBe(true)
    expect(rawHasAttendees('BEGIN:VEVENT\r\nATTENDEE;CN=Bob:mailto:bob@example.org\r\n')).toBe(true)
    expect(rawHasAttendees('BEGIN:VEVENT\r\nSUMMARY:ATTENDEE:\r\n')).toBe(false)
  })
})

describe('tally', () => {
  it('counts answers and treats everything else as not answered', () => {
    expect(
      tally([
        { status: 'accepted' },
        { status: 'accepted' },
        { status: 'tentative' },
        { status: 'declined' },
        { status: 'needs-action' },
        { status: null },
      ]),
    ).toEqual({ accepted: 2, tentative: 1, declined: 1, none: 2 })
  })
})
