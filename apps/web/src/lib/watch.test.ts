import { describe, expect, test } from 'bun:test'

import type { Account } from '@holtmail/api-client'

import { gmailLike, watchBadge, watchMode, watchRequest } from './watch.ts'

function account(overrides: Partial<Account> = {}) {
  return { id: 'a', name: 'Ada', kind: 'imap', enabled: true, ...overrides } as Account
}

describe('watchMode', () => {
  test('defaults to the inbox', () => {
    expect(watchMode(account())).toBe('inbox')
    expect(watchMode(account({ watch: { mode: 'off', folders: [] } }))).toBe('off')
    expect(watchMode(account({ watch: { mode: 'folders', folders: ['Work'] } }))).toBe('folders')
    expect(watchMode(account({ watch: { mode: 'sometimes', folders: [] } }))).toBe('inbox')
  })
})

describe('watchRequest', () => {
  test('sends folders only when chosen', () => {
    expect(watchRequest('folders', 'INBOX, Work\nWork')).toEqual({
      mode: 'folders',
      folders: ['INBOX', 'Work'],
    })
    expect(watchRequest('inbox', 'Work')).toEqual({ mode: 'inbox', folders: [] })
  })
})

describe('gmailLike', () => {
  test('by sign-in or host', () => {
    expect(gmailLike(account({ oauth_provider: 'google' }))).toBe(true)
    expect(
      gmailLike(
        account({
          connection: {
            host: 'IMAP.gmail.com',
            port: 993,
            security: 'tls',
            username: 'ada',
            exclude: [],
          },
        }),
      ),
    ).toBe(true)
    expect(gmailLike(account())).toBe(false)
  })
})

describe('watchBadge', () => {
  test('names each state', () => {
    expect(watchBadge({ state: 'live' }, 'mail')).toMatchObject({ label: 'Live', tone: 'live' })
    expect(
      watchBadge({ state: 'reconnecting', reason: 'Connection lost: timeout.' }, 'contacts'),
    ).toEqual({
      label: 'Reconnecting',
      title: 'Connection lost: timeout.',
      tone: 'warn',
    })
    expect(watchBadge({ state: 'polling' }, 'mail')?.tone).toBe('muted')
    expect(watchBadge({ state: 'off' }, 'mail')).toBeUndefined()
    expect(watchBadge(undefined, 'mail')).toBeUndefined()
  })
})
