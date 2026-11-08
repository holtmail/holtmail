import { describe, expect, test } from 'bun:test'

import type { Account } from '@holtmail/api-client'

import { accountState, anyRunning, fileKind, formatAgo } from './accounts.ts'

const now = Date.UTC(2026, 10, 3, 12)

function account(
  overrides: Partial<Account> = {},
  status: Partial<Account['status']> = {},
): Account {
  return {
    id: 'a',
    name: 'Ada',
    kind: 'imap',
    enabled: true,
    needs_sign_in: false,
    writable: [],
    ...overrides,
    status: {
      running: false,
      new_messages: 0,
      watch: { state: 'off' },
      contacts_watch: { state: 'off' },
      ...status,
    },
  }
}

describe('formatAgo', () => {
  test('rounds down to the largest unit', () => {
    expect(formatAgo(now - 30_000, now, 'en')).toBe('just now')
    expect(formatAgo(now - 5 * 60_000, now, 'en')).toBe('5 minutes ago')
    expect(formatAgo(now - 3 * 3_600_000 - 1, now, 'en')).toBe('3 hours ago')
    expect(formatAgo(now - 86_400_000, now, 'en')).toBe('yesterday')
  })

  test('future times read as now', () => {
    expect(formatAgo(now + 10_000, now, 'en')).toBe('just now')
  })
})

describe('accountState', () => {
  test('a running sync shows progress', () => {
    expect(accountState(account({}, { running: true }), now)).toEqual({
      tone: 'running',
      label: 'Syncing…',
    })
    expect(
      accountState(account({ kind: 'import' }, { running: true, new_messages: 1200 }), now).label,
    ).toBe('Importing… 1,200 new')
  })

  test('errors win over the paused state', () => {
    expect(accountState(account({ enabled: false }, { error: 'Refused' }), now)).toEqual({
      tone: 'error',
      label: 'Refused',
    })
    expect(accountState(account({ enabled: false }), now).tone).toBe('paused')
  })

  test('an expired sign-in wins over the last error', () => {
    expect(
      accountState(account({ needs_sign_in: true, oauth_provider: 'google' }, { error: 'x' }), now),
    ).toEqual({ tone: 'error', label: 'Sign-in expired' })
  })

  test('a missing consent to send asks for another sign-in', () => {
    expect(
      accountState(
        account({
          needs_sign_in: true,
          oauth_provider: 'microsoft',
          sign_in_reason:
            'Sending is turned on, but the sign-in does not allow Microsoft to send mail.',
        }),
        now,
      ),
    ).toEqual({ tone: 'error', label: 'Sign in again to allow it' })
  })

  test('a sync error shows over missing send consent, since syncing goes on', () => {
    expect(
      accountState(
        account(
          {
            needs_sign_in: true,
            sign_in_reason: 'Sending is turned on, but the sign-in does not allow sending.',
          },
          { error: 'mailbox full' },
        ),
        now,
      ),
    ).toEqual({ tone: 'error', label: 'mailbox full' })
  })

  test('finished runs say when and how much', () => {
    const finished = now - 2 * 60_000
    expect(accountState(account({}, { finished_at: finished, new_messages: 1 }), now).label).toBe(
      `Synced ${formatAgo(finished, now)} · 1 new message`,
    )
    expect(
      accountState(
        account({ kind: 'import', enabled: false }, { finished_at: finished, new_messages: 3 }),
        now,
      ).label,
    ).toBe('Imported 3 messages')
    expect(accountState(account(), now).tone).toBe('waiting')
  })

  test('anyRunning looks at every account', () => {
    expect(anyRunning(undefined)).toBe(false)
    expect(anyRunning([account(), account({}, { running: true })])).toBe(true)
  })
})

describe('fileKind', () => {
  test('reads the extension, and treats bare names as mbox', () => {
    expect(fileKind('Inbox')).toBe('mbox')
    expect(fileKind('old.MBOX')).toBe('mbox')
    expect(fileKind('note.eml')).toBe('eml')
    expect(fileKind('takeout.zip')).toBe('zip')
    expect(fileKind('My Contacts.VCF')).toBe('vcf')
    expect(fileKind('team.ics')).toBe('ics')
    expect(fileKind('photo.png')).toBe('unknown')
  })
})
