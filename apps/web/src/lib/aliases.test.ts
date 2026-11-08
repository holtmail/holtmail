import { describe, expect, test } from 'bun:test'

import type { Alias, AliasStrategies, MessageAlias } from '@holtmail/api-client'

import {
  aliasDomain,
  aliasDomains,
  aliasProviders,
  aliasTrust,
  domainKey,
  domainOverrides,
  filterAliases,
  isReplyStrategy,
  providerName,
  serviceStrategies,
  shownFrom,
} from './aliases.ts'

const alias = (address: string, extra: Partial<Alias> = {}): Alias => ({
  address,
  provider: 'simplelogin',
  source: 'service',
  mailboxes: [],
  messages: 0,
  senders: 0,
  signed: 0,
  accounts: [],
  reply_strategy: 'reverse_alias',
  reply_strategy_source: 'default',
  ...extra,
})

const forwarded = (extra: Partial<MessageAlias> = {}): MessageAlias => ({
  provider: 'simplelogin',
  alias: 'shop@alias.example.org',
  signed: false,
  known: false,
  ...extra,
})

describe('filterAliases', () => {
  const all = [
    alias('shop@alias.example.org', { note: 'Furniture shop', mailboxes: ['owner@example.org'] }),
    alias('news@alias.example.org', { source: 'mail', provider: 'addy' }),
    alias('quiet.river@icloud.example.org', {
      source: 'manual',
      provider: 'apple',
      for_domain: 'travel.example.com',
      accounts: [{ account_id: 'a1', name: 'iCloud', messages: 3 }],
    }),
  ]

  const find = (text: string) =>
    filterAliases(all, { text, source: 'all', provider: 'all' }).map((a) => a.address)

  test('every word must match an address, note, website, mailbox or account', () => {
    expect(find('')).toHaveLength(3)
    expect(find('furniture OWNER')).toEqual(['shop@alias.example.org'])
    expect(find('travel icloud')).toEqual(['quiet.river@icloud.example.org'])
    expect(find('shop news')).toEqual([])
  })

  test('narrows by source and provider', () => {
    expect(
      filterAliases(all, { text: '', source: 'mail', provider: 'all' }).map((a) => a.address),
    ).toEqual(['news@alias.example.org'])
    expect(
      filterAliases(all, { text: '', source: 'all', provider: 'apple' }).map((a) => a.address),
    ).toEqual(['quiet.river@icloud.example.org'])
  })
})

describe('shownFrom', () => {
  const from = [{ name: 'Ada - ada at example.com', email: 'ra_1@sl.example.org' }]

  test('the sender a service hid comes first', () => {
    const sender = { name: 'Ada', email: 'ada@example.com' }
    expect(shownFrom({ from, alias: forwarded({ sender }) })).toEqual([sender])
  })

  test('From stays when the service did not hide anyone', () => {
    expect(shownFrom({ from, alias: forwarded() })).toBe(from)
    expect(shownFrom({ from, alias: null })).toBe(from)
  })
})

test('aliasTrust prefers the signature, then a known alias', () => {
  expect(aliasTrust(forwarded({ signed: true }))).toBe('signed')
  expect(aliasTrust(forwarded({ known: true }))).toBe('known')
  expect(aliasTrust(forwarded())).toBe('claimed')
})

test('providers, names and domains', () => {
  const providers = [
    {
      id: 'simplelogin',
      name: 'SimpleLogin',
      key_label: 'API key',
      key_help: '',
      detects: true,
    },
  ]
  expect(providerName(providers, 'simplelogin')).toBe('SimpleLogin')
  expect(providerName(providers, 'own-domain')).toBe('own-domain')
  expect(
    aliasProviders([alias('a@x.example'), alias('b@x.example', { provider: 'addy' })]),
  ).toEqual(['addy', 'simplelogin'])
  expect(aliasDomain('shop@alias.example.org')).toBe('alias.example.org')
  expect(aliasDomain('nobody')).toBe('')
})

describe('reply strategies', () => {
  const strategies: AliasStrategies = {
    defaults: [
      { provider: 'simplelogin', strategy: 'reverse_alias' },
      { provider: 'relay', strategy: 'reply_header' },
      { provider: 'rules', strategy: 'direct' },
    ],
    overrides: [
      { scope: 'alias', key: 'shop@sl.example.org', strategy: 'direct' },
      { scope: 'domain', key: 'mail.example.org', strategy: 'direct' },
      { scope: 'domain', key: 'b.example.org', strategy: 'reverse_alias' },
      { scope: 'service', key: 'relay', strategy: 'direct' },
    ],
  }

  test('lists services in use and with an override, each with its default', () => {
    expect(serviceStrategies(strategies, ['simplelogin', 'simplelogin', ''])).toEqual([
      { provider: 'relay', fallback: 'reply_header', override: 'direct' },
      { provider: 'simplelogin', fallback: 'reverse_alias', override: null },
    ])
    expect(serviceStrategies(strategies, ['unknown'])[1]).toEqual({
      provider: 'unknown',
      fallback: 'direct',
      override: null,
    })
  })

  test('domain overrides and keys', () => {
    expect(domainOverrides(strategies)).toEqual([
      { domain: 'b.example.org', strategy: 'reverse_alias' },
      { domain: 'mail.example.org', strategy: 'direct' },
    ])
    expect(domainKey(' @Mail.Example.org ')).toBe('mail.example.org')
    expect(domainKey('example')).toBeNull()
    expect(domainKey('a b.example.org')).toBeNull()
    expect(isReplyStrategy('reply_header')).toBe(true)
    expect(isReplyStrategy('')).toBe(false)
  })

  test('alias domains are unique and sorted', () => {
    expect(
      aliasDomains([
        alias('x@SL.example.org'),
        alias('y@sl.example.org'),
        alias('z@a.example.net'),
        alias('broken'),
      ]),
    ).toEqual(['a.example.net', 'sl.example.org'])
  })
})
