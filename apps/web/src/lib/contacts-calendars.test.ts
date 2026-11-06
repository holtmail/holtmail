import { describe, expect, test } from 'bun:test'

import type { Account } from '@holtmail/api-client'

import {
  asksConsent,
  contactsState,
  contactsSupport,
  davPreset,
  davRequest,
} from './contacts-calendars.ts'

const now = Date.UTC(2026, 10, 5, 12)

function account(overrides: Partial<Account> = {}, status: Partial<Account['status']> = {}) {
  return {
    id: 'a',
    name: 'Ada',
    kind: 'imap',
    enabled: true,
    needs_sign_in: false,
    contacts_calendars: { enabled: false, needs_consent: false, protocol: 'dav' },
    ...overrides,
    status: { running: false, new_messages: 0, ...status },
  } as Account
}

describe('davPreset', () => {
  test('knows Fastmail, iCloud and Yandex by domain', () => {
    expect(davPreset('ada@fastmail.com')?.calendars).toBe('https://caldav.fastmail.com')
    expect(davPreset('Ada@Me.com')?.contacts).toBe('https://contacts.icloud.com')
    expect(davPreset('ada@ya.ru')?.name).toBe('Yandex')
    expect(davPreset('ada@example.com')).toBeUndefined()
  })
})

describe('contactsSupport', () => {
  test('by kind and sign-in', () => {
    expect(contactsSupport(account())).toBe('dav')
    expect(contactsSupport(account({ oauth_provider: 'google' }))).toBe('google')
    expect(contactsSupport(account({ kind: 'graph', oauth_provider: 'microsoft' }))).toBe('graph')
    expect(contactsSupport(account({ kind: 'jmap' }))).toBe('jmap')
    expect(
      contactsSupport(account({ oauth_provider: 'microsoft', contacts_calendars: null })),
    ).toBe('microsoft-imap')
    expect(contactsSupport(account({ kind: 'import', contacts_calendars: null }))).toBe('none')
  })
  test('Google and Graph accounts ask for consent', () => {
    expect(asksConsent('google')).toBe(true)
    expect(asksConsent('graph')).toBe(true)
    expect(asksConsent('jmap')).toBe(false)
    expect(asksConsent('dav')).toBe(false)
  })
})

describe('davRequest', () => {
  test('empty addresses are left to the server', () => {
    expect(davRequest(true, ' ', '')).toEqual({
      enabled: true,
      contacts_url: null,
      calendars_url: null,
    })
    expect(davRequest(true, ' https://dav.example.com ', '')).toEqual({
      enabled: true,
      contacts_url: 'https://dav.example.com',
      calendars_url: null,
    })
  })
})

describe('contactsState', () => {
  const on = { enabled: true, needs_consent: false, protocol: 'dav' }
  test('nothing when off', () => {
    expect(contactsState(account(), now)).toBeUndefined()
  })
  test('asks for consent first', () => {
    expect(
      contactsState(
        account({ contacts_calendars: { enabled: true, needs_consent: true, protocol: 'dav' } }),
        now,
      )?.tone,
    ).toBe('error')
  })
  test('follows the latest run', () => {
    expect(contactsState(account({ contacts_calendars: on }), now)?.tone).toBe('waiting')
    expect(
      contactsState(
        account({ contacts_calendars: on }, { contacts_calendars: { running: true, summary: '' } }),
        now,
      )?.tone,
    ).toBe('running')
    expect(
      contactsState(
        account(
          { contacts_calendars: on },
          { contacts_calendars: { running: false, summary: '', error: 'refused' } },
        ),
        now,
      )?.label,
    ).toBe('Contacts and calendars: refused')
    expect(
      contactsState(
        account(
          { contacts_calendars: on },
          {
            contacts_calendars: {
              running: false,
              summary: '',
              finished_at: now - 5 * 60_000,
            },
          },
        ),
        now,
      )?.label,
    ).toBe('Contacts and calendars synced 5 minutes ago')
  })
})
