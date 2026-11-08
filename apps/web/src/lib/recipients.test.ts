import { describe, expect, test } from 'bun:test'

import type { PersonSummary } from '@holtmail/api-client'

import {
  addRecipients,
  allRecipients,
  chipLabel,
  formatMailbox,
  isAddress,
  parseMailbox,
  parseRecipients,
  peopleSuggestions,
  removeRecipient,
  splitRecipientText,
} from './recipients.ts'

const person = (name: string | null, emails: string[]) =>
  ({ id: name ?? emails[0], name, email: emails[0] ?? null, emails }) as PersonSummary

describe('recipient chips', () => {
  test('reads bare, named and quoted addresses', () => {
    expect(parseMailbox('ada@example.com')).toEqual({ address: 'ada@example.com' })
    expect(parseMailbox(' Ada Lovelace <ada@example.com> ')).toEqual({
      name: 'Ada Lovelace',
      address: 'ada@example.com',
    })
    expect(parseMailbox('"Lovelace, Ada" <ada@example.com>')).toEqual({
      name: 'Lovelace, Ada',
      address: 'ada@example.com',
    })
    expect(parseMailbox('<bob@example.org>')).toEqual({ address: 'bob@example.org' })
    expect(parseMailbox('mailto:bob@example.org')).toEqual({ address: 'bob@example.org' })
  })

  test('refuses what is no address', () => {
    for (const text of ['', 'ada', 'ada@example', 'ada @example.com', 'Ada <ada>', 'a@b@c.d']) {
      expect(parseMailbox(text)).toBeUndefined()
    }
    expect(isAddress('ada+shop@example.com')).toBe(true)
  })

  test('splits pasted lists outside quotes and brackets', () => {
    expect(
      splitRecipientText('"Lovelace, Ada" <ada@example.com>; bob@example.org,\ncy@example.net'),
    ).toEqual(['"Lovelace, Ada" <ada@example.com>', 'bob@example.org', 'cy@example.net'])
    expect(parseRecipients('ada@example.com, nope, bob@example.org')).toEqual({
      mailboxes: [{ address: 'ada@example.com' }, { address: 'bob@example.org' }],
      invalid: ['nope'],
    })
  })

  test('adds each address once and fills in a missing name', () => {
    const list = addRecipients(
      [{ address: 'Ada@Example.com' }],
      [{ name: 'Ada', address: 'ada@example.com' }, { address: 'bob@example.org' }],
    )
    expect(list).toEqual([
      { name: 'Ada', address: 'Ada@Example.com' },
      { address: 'bob@example.org' },
    ])
    expect(removeRecipient(list, 'ADA@example.com')).toEqual([{ address: 'bob@example.org' }])
    expect(
      allRecipients([{ address: 'a@example.com' }], undefined, [{ address: 'A@example.com' }]),
    ).toHaveLength(1)
  })

  test('labels and formats chips', () => {
    expect(chipLabel({ name: ' ', address: 'ada@example.com' })).toBe('ada@example.com')
    expect(chipLabel({ name: 'Ada', address: 'ada@example.com' })).toBe('Ada')
    expect(formatMailbox({ name: 'Lovelace, Ada', address: 'ada@example.com' })).toBe(
      '"Lovelace, Ada" <ada@example.com>',
    )
    expect(formatMailbox({ name: 'Ada', address: 'ada@example.com' })).toBe('Ada <ada@example.com>')
  })

  test('suggests people by name or address, without chosen ones', () => {
    const people = [
      person('Ada Lovelace', ['ada@example.com', 'ada@work.example.org']),
      person(null, ['adams@example.net']),
      person('Bob', ['bob@example.org']),
    ]
    expect(peopleSuggestions(people, 'ada', [{ address: 'ADA@example.com' }])).toEqual([
      { name: 'Ada Lovelace', address: 'ada@work.example.org' },
      { address: 'adams@example.net' },
    ])
    expect(peopleSuggestions(people, '  ', [])).toEqual([])
    expect(peopleSuggestions(people, 'example', [], 2)).toHaveLength(2)
  })
})
