import { describe, expect, test } from 'bun:test'

import type { Collection, EventForm, PimChange } from '@holtmail/api-client'

import { editStrings } from '../strings-edits.ts'
import {
  addDays,
  addMinutes,
  asksToConfirm,
  cardEditable,
  changeActions,
  composedName,
  contactDraft,
  contactRequest,
  decisionCount,
  editableCollections,
  emptyAddress,
  eventDraft,
  eventRequest,
  groupByCollection,
  markText,
  modeChoices,
  newEventDraft,
  newSender,
  openCounts,
  rawTemplate,
  repeatOf,
  senderDraft,
  withNamePart,
  withStart,
  writeModes,
} from './edits.ts'

function collection(id: string, overrides: Partial<Collection> = {}): Collection {
  return {
    id,
    archive_id: 'a',
    account_id: 'acc',
    account_name: 'Work',
    synced_at: null,
    kind: 'calendar',
    name: id,
    color: null,
    created_at: 0,
    removed_at: null,
    items: 0,
    removed_items: 0,
    write_mode: 'held',
    editable: true,
    ...overrides,
  }
}

function change(id: string, overrides: Partial<PimChange> = {}): PimChange {
  return {
    id,
    archive_id: 'a',
    collection_id: 'cal',
    item_id: 'item',
    href: null,
    op: 'update',
    class: 'edit',
    origin: 'api',
    state: 'held',
    reason: null,
    upstream_changed: false,
    upstream_deleted: false,
    attempts: 0,
    last_error: null,
    next_attempt_at: null,
    user_id: null,
    created_at: 1,
    updated_at: 1,
    title: null,
    ...overrides,
  }
}

const form = (overrides: Partial<EventForm> = {}): EventForm => ({
  uid: 'standup@example.com',
  title: 'Standup',
  start: '2026-01-05T09:00:00',
  end: '2026-01-05T09:30:00',
  all_day: false,
  time_zone: 'Europe/Berlin',
  recurrence: null,
  location: null,
  description: null,
  organizer: null,
  attendees: [],
  alarms: [],
  ...overrides,
})

describe('collections', () => {
  test('only editable, present collections of the kind are offered, by name', () => {
    const list = [
      collection('b'),
      collection('a'),
      collection('ro', { editable: false }),
      collection('gone', { removed_at: 5 }),
      collection('book', { kind: 'contacts' }),
    ]
    expect(editableCollections(list, 'event').map((c) => c.id)).toEqual(['a', 'b'])
    expect(editableCollections(list, 'contact').map((c) => c.id)).toEqual(['book'])
    expect(editableCollections(undefined, 'event')).toEqual([])
  })

  test('the picker offers only the modes the server allows', () => {
    expect(modeChoices({ account_id: null, write_mode: 'read_only' }, writeModes)).toEqual([
      'read_only',
    ])
    expect(
      modeChoices({ account_id: 'acc', write_mode: 'held' }, [
        'read_only',
        'held',
        'write_through',
      ]),
    ).toEqual(['read_only', 'held', 'write_through'])
    expect(modeChoices({ account_id: 'acc', write_mode: 'held' }, ['read_only', 'held'])).toEqual([
      'read_only',
      'held',
    ])
    expect(modeChoices({ account_id: 'imp', write_mode: 'read_only' }, ['read_only'])).toEqual([
      'read_only',
    ])
    expect(modeChoices({ account_id: 'acc', write_mode: 'held' })).toEqual(['held'])
  })

  test('turning on sending asks first', () => {
    expect(asksToConfirm('held', 'write_through')).toBe(true)
    expect(asksToConfirm('read_only', 'write_through')).toBe(true)
    expect(asksToConfirm('write_through', 'write_through')).toBe(false)
    expect(asksToConfirm('write_through', 'held')).toBe(false)
  })

  test('a person card is editable through its primary card, else a new card', () => {
    const books = [collection('book', { kind: 'contacts' }), collection('ro', { editable: false })]
    const cards = [{ item_id: 'c1', collection_id: 'ro' }]
    expect(cardEditable({ primary_card: 'c1', cards }, books)).toBe(false)
    expect(cardEditable({ primary_card: null, cards }, books)).toBe(true)
    expect(cardEditable({ primary_card: null, cards }, [])).toBe(false)
  })
})

describe('marks and changes', () => {
  const s = editStrings.en.mark

  test('the four states read as CLIENTS.md says', () => {
    expect(markText({ state: 'held', account_name: 'Fastmail' }, s)).toBe('Not sent to Fastmail')
    expect(markText({ state: 'sending', account_name: 'Fastmail' }, s)).toBe('Sending')
    expect(markText({ state: 'conflict', account_name: 'Fastmail' }, s)).toBe(
      'Changed in Fastmail too',
    )
    expect(markText({ state: 'rejected', account_name: null }, s)).toBe('the account refused')
    expect(markText({ state: 'applied', account_name: 'Fastmail' }, s)).toBeUndefined()
    expect(markText({ state: 'held', account_name: 'Fastmail' }, editStrings.ru.mark)).toBe(
      'Не отправлено в Fastmail',
    )
  })

  test('actions follow the state', () => {
    expect(changeActions(change('1', { state: 'conflict' }))).toEqual([
      'keep_mine',
      'take_theirs',
      'discard',
    ])
    expect(changeActions(change('1', { state: 'conflict', item_id: null }))).toEqual(['discard'])
    expect(changeActions(change('1', { state: 'rejected' }))).toEqual(['retry', 'discard'])
    expect(changeActions(change('1', { state: 'held' }))).toEqual(['discard'])
    expect(changeActions(change('1', { state: 'sending' }))).toEqual([])
  })

  test('conflicts and refusals need a decision', () => {
    const list = [
      change('1', { state: 'conflict' }),
      change('2', { state: 'rejected' }),
      change('3', { state: 'held' }),
    ]
    expect(decisionCount(list)).toBe(2)
    expect(decisionCount(undefined)).toBe(0)
  })

  test('changes group by collection name, oldest first, unknown collections last', () => {
    const groups = groupByCollection(
      [
        change('3', { collection_id: 'z', created_at: 3 }),
        change('2', { collection_id: 'gone', created_at: 1 }),
        change('1', { collection_id: 'z', created_at: 1 }),
        change('4', { collection_id: 'b', created_at: 2 }),
      ],
      [collection('z', { name: 'Zeta' }), collection('b', { name: 'Beta' })],
    )
    expect(groups.map((g) => g.collectionId)).toEqual(['b', 'z', 'gone'])
    expect(groups[1]?.changes.map((c) => c.id)).toEqual(['1', '3'])
    expect(openCounts([change('1'), change('2'), change('3', { collection_id: 'b' })])).toEqual(
      new Map([
        ['cal', 2],
        ['b', 1],
      ]),
    )
  })
})

describe('dates', () => {
  test('days and minutes move on the calendar', () => {
    expect(addDays('2026-02-28', 1)).toBe('2026-03-01')
    expect(addDays('2024-03-01', -1)).toBe('2024-02-29')
    expect(addMinutes('2026-01-05', '23:30', 60)).toEqual(['2026-01-06', '00:30'])
    expect(addDays('nonsense', 1)).toBe('nonsense')
  })
})

describe('event drafts', () => {
  test('a new event starts at the next full hour today, else at nine', () => {
    const now = new Date(2026, 0, 5, 14, 20)
    const today = newEventDraft('2026-01-05', 'Europe/Berlin', now)
    expect([today.startDate, today.startTime, today.endDate, today.endTime]).toEqual([
      '2026-01-05',
      '15:00',
      '2026-01-05',
      '16:00',
    ])
    const later = newEventDraft('2026-01-09', 'UTC', now)
    expect([later.startTime, later.endTime, later.timeZone, later.alarms]).toEqual([
      '09:00',
      '10:00',
      'UTC',
      [15],
    ])
  })

  test('an unchanged form keeps the times exactly as served', () => {
    const original = form({ recurrence: 'FREQ=WEEKLY;BYDAY=MO', alarms: [{ minutes_before: 10 }] })
    const result = eventRequest(eventDraft(original), original)
    expect(result).toEqual({ form: original })
  })

  test('a missing end stays missing until it is changed', () => {
    const original = form({ end: null })
    const draft = eventDraft(original)
    expect([draft.endDate, draft.endTime]).toEqual(['2026-01-05', '10:00'])
    const kept = eventRequest(draft, original)
    expect('form' in kept && kept.form.end).toBeNull()
    const moved = eventRequest({ ...draft, endTime: '11:00' }, original)
    expect('form' in moved && moved.form.end).toBe('2026-01-05T11:00')
  })

  test('all-day ends are inclusive in the form and exclusive on the wire', () => {
    const original = form({
      all_day: true,
      start: '2026-01-05',
      end: '2026-01-07',
      time_zone: null,
    })
    const draft = eventDraft(original)
    expect([draft.startDate, draft.endDate]).toEqual(['2026-01-05', '2026-01-06'])
    const result = eventRequest({ ...draft, endDate: '2026-01-08' }, original)
    expect('form' in result && [result.form.start, result.form.end]).toEqual([
      '2026-01-05',
      '2026-01-09',
    ])
  })

  test('moving the start keeps the length', () => {
    const draft = eventDraft(form())
    const moved = withStart(draft, '2026-01-06', '23:45')
    expect([moved.endDate, moved.endTime]).toEqual(['2026-01-07', '00:15'])
    const allDay = eventDraft(form({ all_day: true, start: '2026-01-05', end: '2026-01-08' }))
    expect(withStart(allDay, '2026-02-01', '').endDate).toBe('2026-02-03')
  })

  test('problems are named', () => {
    const draft = eventDraft(form())
    expect(eventRequest({ ...draft, startDate: '' })).toEqual({ problem: 'start' })
    expect(eventRequest({ ...draft, endTime: '08:00' })).toEqual({ problem: 'endBefore' })
    expect(
      eventRequest({ ...draft, attendees: [{ email: 'bob', name: null, partstat: null }] }),
    ).toEqual({ problem: 'email', value: 'bob' })
    expect(eventRequest({ ...draft, repeat: 'custom', rule: ' ' })).toEqual({ problem: 'rule' })
  })

  test('repeats map to rules', () => {
    expect(repeatOf(null)).toBe('none')
    expect(repeatOf('FREQ=WEEKLY')).toBe('weekly')
    expect(repeatOf('FREQ=WEEKLY;BYDAY=MO')).toBe('custom')
    const result = eventRequest({ ...eventDraft(form()), repeat: 'monthly' })
    expect('form' in result && result.form.recurrence).toBe('FREQ=MONTHLY')
  })

  test('new events float without a zone and drop empty fields', () => {
    const result = eventRequest({
      ...newEventDraft('2026-03-01', '', new Date(2026, 0, 1)),
      title: '  Lunch ',
      location: ' ',
      alarms: [15, 15, 60],
    })
    expect('form' in result && result.form).toMatchObject({
      title: 'Lunch',
      start: '2026-03-01T09:00',
      end: '2026-03-01T10:00',
      time_zone: null,
      location: null,
      alarms: [{ minutes_before: 15 }, { minutes_before: 60 }],
    })
  })
})

describe('contact drafts', () => {
  test('a form round-trips, keeping kinds the form has no label for', () => {
    const original = {
      uid: 'ada@example.com',
      full_name: 'Ada Quill',
      name: { given: 'Ada', family: 'Quill', additional: null, prefix: null, suffix: null },
      emails: [{ value: 'ada@example.com', kinds: ['pref', 'work'] }],
      phones: [{ value: '+15550100', kinds: ['cell'] }],
      addresses: [
        {
          kinds: ['home'],
          po_box: null,
          extended: null,
          street: '1 Example Road',
          locality: 'Exampleton',
          region: null,
          postal_code: '12345',
          country: null,
        },
      ],
      organization: 'Example Ltd',
      title: null,
      notes: null,
    }
    const draft = contactDraft(original)
    expect(draft.emails[0]).toEqual({ value: 'ada@example.com', label: 'work', extra: ['pref'] })
    const result = contactRequest(draft)
    expect('form' in result && result.form.emails[0]?.kinds).toEqual(['work', 'pref'])
    expect('form' in result && result.form.addresses).toEqual(original.addresses)
    expect('form' in result && result.form.full_name).toBe('Ada Quill')
  })

  test('the shown name follows the parts until it is changed by hand', () => {
    let draft = withNamePart(contactDraft(), 'given', 'Bea')
    draft = withNamePart(draft, 'family', 'Example')
    expect(draft.fullName).toBe('Bea Example')
    draft = { ...draft, fullName: 'Dr. B.' }
    expect(withNamePart(draft, 'given', 'Beatrice').fullName).toBe('Dr. B.')
    expect(
      composedName({ prefix: 'Dr.', given: 'Bea', additional: '', family: 'X', suffix: '' }),
    ).toBe('Dr. Bea X')
  })

  test('empty rows are dropped and an empty contact is refused', () => {
    const draft = {
      ...contactDraft(),
      emails: [{ value: ' ', label: '', extra: [] }],
      addresses: [{ ...emptyAddress(), label: 'home' }],
    }
    expect(contactRequest(draft)).toEqual({ problem: 'empty' })
    const named = contactRequest({ ...draft, organization: 'Example Ltd' })
    expect('form' in named && [named.form.emails, named.form.addresses]).toEqual([[], []])
    expect(
      contactRequest({ ...draft, emails: [{ value: 'not-an-address', label: '', extra: [] }] }),
    ).toEqual({ problem: 'email', value: 'not-an-address' })
  })

  test('a sender becomes a draft with their name and address', () => {
    const draft = senderDraft('Bea van Example', 'bea@example.org')
    expect([draft.fullName, draft.given, draft.family]).toEqual([
      'Bea van Example',
      'Bea van',
      'Example',
    ])
    expect(draft.emails).toEqual([{ value: 'bea@example.org', label: '', extra: [] }])
    expect(senderDraft(null, 'x@example.org').fullName).toBe('')
  })

  test('a sender who is already a person is not offered', () => {
    const from = [{ email: 'Bea@Example.org', name: 'Bea' }]
    expect(newSender(from, [{ email: 'bea@example.org', has_card: true }])).toBeUndefined()
    expect(newSender(from, [{ email: 'Bea@example.org', has_card: false }])).toEqual(from[0])
    expect(newSender(from, [])).toEqual(from[0])
    expect(newSender([], [])).toBeUndefined()
  })
})

test('raw templates carry the UID', () => {
  const now = new Date(Date.UTC(2026, 0, 5, 9, 0, 0))
  expect(rawTemplate('contact', 'u1', now)).toContain('UID:u1\r\n')
  const event = rawTemplate('event', 'u2', now)
  expect(event).toContain('DTSTAMP:20260105T090000Z')
  expect(event).toContain('DTSTART;VALUE=DATE:20260105')
})
