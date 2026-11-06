import { describe, expect, test } from 'bun:test'

import type { Collection, PersonSummary } from '@holtmail/api-client'

import {
  addressPeople,
  differingFields,
  initials,
  canSplit,
  joinCandidates,
  linkReason,
  overrideNotes,
  personFieldLabel,
  personFor,
  personLabel,
  readContact,
  seenLabel,
  sourceLabel,
  sourceOptions,
  toggleChoice,
  yearBars,
} from './people.ts'

describe('labels', () => {
  test('names a person by name, else address, else phone', () => {
    expect(personLabel({ name: 'Ada Quill', email: 'ada@example.com' }, '?')).toBe('Ada Quill')
    expect(personLabel({ name: '  ', email: 'ada@example.com' }, '?')).toBe('ada@example.com')
    expect(personLabel({ phone: '+15550100' }, '?')).toBe('+15550100')
    expect(personLabel({}, '(no name)')).toBe('(no name)')
  })

  test('takes initials from the first and last word', () => {
    expect(initials('Ada Byron Quill')).toBe('AQ')
    expect(initials('bob')).toBe('B')
    expect(initials('ada@example.com')).toBe('A')
    expect(initials('Élodie  Durand')).toBe('ÉD')
    expect(initials('+1 555 0100')).toBe('')
    expect(initials('')).toBe('')
  })

  test('names a source by account and address book', () => {
    expect(sourceLabel({ account_name: 'Work', collection_name: 'Contacts' })).toBe(
      'Work · Contacts',
    )
    expect(sourceLabel({ account_name: null, collection_name: 'people.vcf' })).toBe('people.vcf')
    expect(sourceLabel({ account_name: 'Home', collection_name: 'Home' })).toBe('Home')
  })

  test('labels fields and the reasons cards were joined', () => {
    expect(personFieldLabel('organization')).toBe('Organization')
    expect(personFieldLabel('pronouns')).toBe('Pronouns')
    expect(linkReason({ kind: 'email', value: 'ada@example.com' })).toBe(
      'same email ada@example.com',
    )
    expect(linkReason({ kind: 'phone', value: '+15550100' })).toBe('same phone +15550100')
    expect(linkReason({ kind: 'hand', value: 'o1' })).toBe('joined by hand')
    expect(
      differingFields([
        { name: 'phone', differs: true, values: [] },
        { name: 'email', differs: false, values: [] },
      ]),
    ).toEqual(['Phone'])
  })
})

describe('seenLabel', () => {
  const now = new Date(2025, 10, 5, 12).getTime()

  test('says how recently a source was seen', () => {
    expect(seenLabel(now - 3_600_000, false, now, 'en-US')).toBe('seen today')
    expect(seenLabel(now - 86_400_000, false, now, 'en-US')).toBe('seen yesterday')
    expect(seenLabel(new Date(2025, 2, 4).getTime(), false, now, 'en-US')).toBe('seen Mar 4')
    expect(seenLabel(new Date(2024, 2, 4).getTime(), false, now, 'en-US')).toBe('last seen 2024')
  })

  test('says when a removed source went away', () => {
    expect(seenLabel(new Date(2024, 2, 4).getTime(), true, now, 'en-US')).toBe(
      'removed Mar 4, 2024',
    )
  })
})

describe('addressPeople', () => {
  test('finds people by address without regard to case', () => {
    const people = addressPeople([{ email: 'Ada@Example.com', person_id: 'p1' }])
    expect(personFor(people, ' ada@example.COM ')).toBe('p1')
    expect(personFor(people, 'bob@example.org')).toBeUndefined()
  })
})

describe('yearBars', () => {
  test('fills the years between with zero and scales to the busiest', () => {
    expect(
      yearBars([
        { year: 2021, messages: 4 },
        { year: 2023, messages: 2 },
      ]),
    ).toEqual([
      { year: 2021, messages: 4, share: 1 },
      { year: 2022, messages: 0, share: 0 },
      { year: 2023, messages: 2, share: 0.5 },
    ])
    expect(yearBars([])).toEqual([])
  })
})

function collection(
  id: string,
  kind: string,
  name: string,
  account: string | null,
  items: number,
): Collection {
  return {
    id,
    archive_id: 'a',
    account_id: null,
    account_name: account,
    synced_at: null,
    kind,
    name,
    color: null,
    created_at: 0,
    removed_at: null,
    items,
    removed_items: 0,
  }
}

describe('sourceOptions', () => {
  test('lists the address books with cards after the fixed choices', () => {
    const options = sourceOptions(
      [
        collection('c1', 'contacts', 'Contacts', 'Work', 3),
        collection('c2', 'calendar', 'Team', 'Work', 3),
        collection('c3', 'contacts', 'Empty', null, 0),
      ],
      { all: 'Everyone', contacts: 'With a card', mail: 'From mail' },
    )
    expect(options.map((o) => o.value)).toEqual(['all', 'contacts', 'mail', 'c1'])
    expect(options[3]?.label).toBe('Work · Contacts')
  })
})

describe('readContact', () => {
  test('reads the normalized view of a card', () => {
    expect(
      readContact({
        type: 'contact',
        full_name: null,
        name: { given: 'Ada', family: 'Quill' },
        emails: [{ address: 'ada@example.com', normalized: 'ada@example.com', kinds: [] }],
        phones: [{ number: '+1 555 0100', e164: '+15550100', kinds: ['cell'] }],
        organization: 'Lighthouse Co',
      }),
    ).toEqual({
      name: 'Ada Quill',
      emails: ['ada@example.com'],
      phones: ['+1 555 0100'],
      organization: 'Lighthouse Co',
      title: undefined,
      birthday: undefined,
      note: undefined,
    })
    expect(readContact({ type: 'calendar' })).toBeUndefined()
    expect(readContact(null)).toBeUndefined()
  })
})

const person = (id: string): PersonSummary => ({
  id,
  archive_id: 'a1',
  name: id,
  email: null,
  phone: null,
  emails: [],
  phones: [],
  cards: 1,
  removed_cards: 0,
  collection_ids: [],
  messages: 0,
  from_me: 0,
  first_message_at: null,
  last_message_at: null,
  differences: false,
  removed: false,
})

describe('splits and joins by hand', () => {
  test('a split leaves a card on both sides', () => {
    expect(canSplit(3, 0)).toBe(false)
    expect(canSplit(3, 1)).toBe(true)
    expect(canSplit(3, 2)).toBe(true)
    expect(canSplit(3, 3)).toBe(false)
    expect(canSplit(1, 1)).toBe(false)
  })

  test('chooses cards in order', () => {
    expect(toggleChoice([], 'c1')).toEqual(['c1'])
    expect(toggleChoice(['c1', 'c2'], 'c1')).toEqual(['c2'])
    expect(toggleChoice(['c2'], 'c1')).toEqual(['c2', 'c1'])
  })

  test('offers everyone but the person once', () => {
    const people = ['p1', 'p2', 'p2', 'p3', 'p4'].map(person)
    expect(joinCandidates(people, 'p1').map((p) => p.id)).toEqual(['p2', 'p3', 'p4'])
    expect(joinCandidates(people, 'p3', 2).map((p) => p.id)).toEqual(['p1', 'p2'])
  })

  test('describes overrides from the side of the person', () => {
    expect(
      overrideNotes(
        [
          {
            id: 'o1',
            kind: 'split',
            keys: ['item:c2'],
            from: ['item:c1'],
            created_at: 5,
            created_by: null,
            people: ['p1', 'p2'],
          },
          {
            id: 'o2',
            kind: 'join',
            keys: ['item:c1', 'email:ada@example.com'],
            from: [],
            created_at: 6,
            created_by: 'u1',
            people: ['p1'],
          },
        ],
        'p1',
      ),
    ).toEqual([
      { kind: 'split', id: 'o1', createdAt: 5, others: ['p2'] },
      { kind: 'joined', id: 'o2', createdAt: 6, others: [] },
    ])
  })
})
