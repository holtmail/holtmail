import { describe, expect, test } from 'bun:test'

import type { Account } from '@holtmail/api-client'

import {
  graphSignIn,
  looksLikeSignInAnswer,
  offeredProviders,
  oauthProviderFor,
  oauthProviderForHost,
  parseOAuthReturn,
  switchableProvider,
  withoutOAuthParams,
} from './oauth.ts'

function account(overrides: Partial<Account> = {}): Account {
  return {
    id: 'a',
    name: 'Ada',
    kind: 'imap',
    enabled: true,
    needs_sign_in: false,
    status: {
      running: false,
      new_messages: 0,
      watch: { state: 'off' },
      contacts_watch: { state: 'off' },
    },
    ...overrides,
  }
}

const connection = (host: string) => ({
  host,
  port: 993,
  security: 'tls',
  username: 'ada@example.com',
  exclude: [],
})

describe('oauthProviderFor', () => {
  test('knows Google and Microsoft domains, ignoring case and spaces', () => {
    expect(oauthProviderFor('ada@gmail.com')).toBe('google')
    expect(oauthProviderFor(' Ada@GoogleMail.com ')).toBe('google')
    expect(oauthProviderFor('ada@outlook.com')).toBe('microsoft')
    expect(oauthProviderFor('ada@hotmail.com')).toBe('microsoft')
    expect(oauthProviderFor('ada@live.com')).toBe('microsoft')
    expect(oauthProviderFor('ada@msn.com')).toBe('microsoft')
    expect(oauthProviderFor('ada@office365.com')).toBe('microsoft')
  })

  test('other or incomplete addresses have none', () => {
    expect(oauthProviderFor('ada@example.com')).toBeUndefined()
    expect(oauthProviderFor('ada@icloud.com')).toBeUndefined()
    expect(oauthProviderFor('ada')).toBeUndefined()
    expect(oauthProviderFor('')).toBeUndefined()
  })
})

describe('switchableProvider', () => {
  test('password accounts on Gmail or Microsoft servers can switch', () => {
    expect(oauthProviderForHost(' IMAP.gmail.com ')).toBe('google')
    expect(switchableProvider(account({ connection: connection('imap.gmail.com') }))).toBe('google')
    expect(switchableProvider(account({ connection: connection('outlook.office365.com') }))).toBe(
      'microsoft',
    )
  })

  test('OAuth accounts, other servers and imports cannot', () => {
    expect(
      switchableProvider(
        account({ connection: connection('imap.gmail.com'), oauth_provider: 'google' }),
      ),
    ).toBeUndefined()
    expect(switchableProvider(account({ connection: connection('imap.example.com') }))).toBe(
      undefined,
    )
    expect(switchableProvider(account({ kind: 'import' }))).toBeUndefined()
    expect(switchableProvider(account())).toBeUndefined()
  })
})

describe('looksLikeSignInAnswer', () => {
  test('accepts an address or query with code and state, or an error', () => {
    expect(looksLikeSignInAnswer('http://localhost/?code=abc&state=xyz')).toBe(true)
    expect(looksLikeSignInAnswer('  https://example.org/cb?state=xyz&scope=a&code=abc ')).toBe(true)
    expect(looksLikeSignInAnswer('code=abc&state=xyz')).toBe(true)
    expect(looksLikeSignInAnswer('http://localhost/?error=access_denied&state=xyz')).toBe(true)
  })

  test('rejects anything else', () => {
    expect(looksLikeSignInAnswer('')).toBe(false)
    expect(looksLikeSignInAnswer('http://localhost/?code=abc')).toBe(false)
    expect(looksLikeSignInAnswer('http://localhost/?zipcode=1&state=x')).toBe(false)
    expect(looksLikeSignInAnswer('hello')).toBe(false)
  })
})

describe('parseOAuthReturn', () => {
  test('reads a finished sign-in and its account', () => {
    expect(parseOAuthReturn('?oauth=done&account=acc1')).toEqual({ kind: 'done', account: 'acc1' })
  })

  test('reads an error, which wins', () => {
    expect(parseOAuthReturn('?oauth_error=Access%20was%20refused')).toEqual({
      kind: 'error',
      message: 'Access was refused',
    })
    expect(parseOAuthReturn('?oauth=done&account=a&oauth_error=')).toEqual({
      kind: 'error',
      message: 'The sign-in failed.',
    })
  })

  test('ignores pages without a sign-in answer', () => {
    expect(parseOAuthReturn('')).toBeUndefined()
    expect(parseOAuthReturn('?oauth=done')).toBeUndefined()
    expect(parseOAuthReturn('?account=a')).toBeUndefined()
  })
})

describe('withoutOAuthParams', () => {
  test('drops the sign-in params and keeps the rest', () => {
    expect(withoutOAuthParams('?oauth=done&account=a')).toBe('')
    expect(withoutOAuthParams('?archive=x&oauth_error=nope')).toBe('?archive=x')
  })

  test('leaves other searches alone', () => {
    expect(withoutOAuthParams('')).toBe('')
    expect(withoutOAuthParams('?account=a')).toBe('?account=a')
  })
})

test('builds a Graph sign-in for any Microsoft address', () => {
  expect(graphSignIn('arc', ' ada@contoso.example.com ', '')).toEqual({
    provider: 'microsoft',
    kind: 'graph',
    archive_id: 'arc',
    email: 'ada@contoso.example.com',
    name: null,
  })
  expect(graphSignIn('arc', 'ada@example.com', ' Work ')?.name).toBe('Work')
  expect(graphSignIn('arc', 'ada', '')).toBeUndefined()
  expect(graphSignIn('arc', 'ada @example.com', '')).toBeUndefined()
})

describe('offered providers', () => {
  test('a provider the server leaves out is not offered', () => {
    expect(offeredProviders(undefined)).toEqual(['google', 'microsoft'])
    expect(offeredProviders([{ provider: 'microsoft' }])).toEqual(['microsoft'])
    expect(oauthProviderFor('ada@gmail.com', ['microsoft'])).toBeUndefined()
    expect(oauthProviderFor('ada@outlook.com', ['microsoft'])).toBe('microsoft')
    const gmail = account({ connection: connection('imap.gmail.com') })
    expect(switchableProvider(gmail, ['microsoft'])).toBeUndefined()
    expect(switchableProvider(gmail)).toBe('google')
  })
})
