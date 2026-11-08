import { describe, expect, test } from 'bun:test'

import type { Identity, Leak, Route } from '@holtmail/api-client'

import { writingStrings } from '../strings-writing.ts'
import {
  aliasIdentity,
  leakFixes,
  leakText,
  reasonText,
  routeErrorText,
  routeFrom,
  routeSummary,
  sendableDespite,
  withoutRecipient,
} from './route.ts'

const en = writingStrings.en
const ru = writingStrings.ru

const route: Route = {
  identity: 'i1',
  from: 'ada@example.com',
  name: 'Ada',
  transport: 't1',
  strategy: 'direct',
  reasons: [{ code: 'original_recipient', address: 'ada@example.com' }],
}

const names = (id: string) => (id === 't1' ? 'Company relay' : undefined)

describe('route explanation', () => {
  test('names the address and the transport', () => {
    expect(routeSummary(route, 'Fastmail SMTP', en)).toBe(
      'From ada@example.com through Fastmail SMTP',
    )
    expect(routeSummary(route, undefined, en)).toBe('From ada@example.com through t1')
    expect(routeSummary({ ...route, strategy: 'reverse_alias' }, 'Relay', en)).toContain(
      'reverse address',
    )
    expect(routeSummary(route, 'Fastmail SMTP', ru)).toBe('От ada@example.com через Fastmail SMTP')
    expect(routeFrom(route)).toBe('Ada <ada@example.com>')
    expect(routeFrom({ ...route, name: null })).toBe('ada@example.com')
  })

  test('explains every reason code', () => {
    expect(reasonText({ code: 'chosen' }, en)).toBe('You picked this identity')
    expect(reasonText({ code: 'original_recipient', address: 'ada@example.com' }, en)).toBe(
      'The message was sent to ada@example.com',
    )
    expect(reasonText({ code: 'plus_address', address: 'ada+shop@example.com' }, en)).toContain(
      'ada+shop@example.com',
    )
    expect(reasonText({ code: 'alias', alias: 'x7@alias.example.net' }, en)).toBe(
      'It came through the alias x7@alias.example.net',
    )
    expect(reasonText({ code: 'account' }, en)).toContain('account')
    expect(reasonText({ code: 'default' }, en)).toBe('It is your default identity')
    expect(reasonText({ code: 'plus_address_dropped' }, en)).toContain('plain address')
    expect(reasonText({ code: 'something_new' }, en)).toBe('Picked by a sending rule')
    expect(reasonText({ code: 'default' }, ru)).toBe('Это ваш основной отправитель')
  })

  test('explains why there is no route', () => {
    expect(routeErrorText({ code: 'no_identity' }, names, en)).toContain('Settings → Sending')
    expect(
      routeErrorText(
        { code: 'not_allowed_from', transport: 't1', address: 'eve@example.org' },
        names,
        en,
      ),
    ).toBe('Company relay may not send as eve@example.org.')
    expect(
      routeErrorText(
        { code: 'not_allowed_from', transport: 't9', address: 'e@example.org' },
        names,
        en,
      ),
    ).toBe('t9 may not send as e@example.org.')
    expect(
      routeErrorText({ code: 'no_alias_identity', alias: 'shop@alias.example' }, names, en),
    ).toContain('none of your identities sends as shop@alias.example')
  })
})

describe('alias replies', () => {
  test('explains the alias reasons', () => {
    expect(reasonText({ code: 'reverse_alias', address: 'ra_bob@sl.example.org' }, en)).toBe(
      'To the reverse address ra_bob@sl.example.org',
    )
    expect(reasonText({ code: 'alias_mailbox', address: 'ada@example.com' }, ru)).toContain(
      'ada@example.com',
    )
    for (const code of [
      'no_alias_mailbox',
      'not_alias_mailbox',
      'alias_from',
      'computed_reverse_alias',
      'unsigned_reverse_alias',
      'reply_header',
    ]) {
      expect(reasonText({ code, address: 'a@example.org', alias: 'b@example.org' }, en)).not.toBe(
        en.route.unknownReason,
      )
    }
    expect(
      routeErrorText({ code: 'needs_reverse_alias', alias: 'shop@sl.example.org' }, names, en),
    ).toContain('shop@sl.example.org')
    expect(sendableDespite({ code: 'needs_reverse_alias' })).toBe(true)
    expect(sendableDespite({ code: 'no_identity' })).toBe(false)
    expect(sendableDespite(null)).toBe(false)
  })

  test('explains leaks and removes a recipient', () => {
    const leak: Leak = {
      recipient: 'carol@example.org',
      field: 'cc',
      reason: 'cc_not_reverse_alias',
      fix: 'remove_recipient',
    }
    expect(leakText(leak, en)).toBe('carol@example.org in Cc is not a reverse address')
    expect(leakText({ ...leak, reason: 'mixed_recipients' }, ru)).toContain('carol@example.org')
    expect(leakText({ ...leak, reason: 'anything' }, en)).toContain('real address')
    expect(
      withoutRecipient(
        [{ address: 'Carol@Example.org' }, { address: 'ra_bob@sl.example.org' }],
        'carol@example.org',
      ),
    ).toEqual([{ address: 'ra_bob@sl.example.org' }])
  })
})

const identity = (id: string, address: string, extra: Partial<Identity> = {}): Identity =>
  ({ id, address, active: true, users: [], ...extra }) as Identity

describe('alias paths', () => {
  const reply: Route = {
    ...route,
    strategy: 'reverse_alias',
    alias: 'shop@sl.example.org',
    to: 'ra_bob@sl.example.org',
    provider: 'simplelogin',
    contact: 'bob@example.org',
  }
  test('says where a reply through an alias service goes and from which mailbox', () => {
    expect(routeSummary(reply, 'Fastmail SMTP', en)).toBe(
      'To SimpleLogin’s reverse address for bob@example.org, from your mailbox ada@example.com through Fastmail SMTP',
    )
    expect(routeSummary({ ...reply, provider: null }, 'Fastmail SMTP', en)).toContain(
      'the alias service’s reverse address for bob@example.org',
    )
    expect(routeSummary(reply, 'Fastmail SMTP', ru)).toBe(
      'На обратный адрес SimpleLogin для bob@example.org, с вашего ящика ada@example.com через Fastmail SMTP',
    )
    expect(
      routeSummary(
        { ...reply, strategy: 'reply_header', provider: 'relay', to: 'r1@relay.example.net' },
        'iCloud',
        en,
      ),
    ).toBe(
      'To r1@relay.example.net, where Firefox Relay asked replies to go, from your mailbox ada@example.com through iCloud',
    )
    expect(
      routeSummary(
        { ...reply, strategy: 'direct', from: 'Shop@sl.example.org', to: null },
        'Own domain',
        en,
      ),
    ).toBe('From the alias Shop@sl.example.org through Own domain')
  })

  test('offers each leak its one-click fixes', () => {
    const leak: Leak = {
      recipient: 'carol@example.org',
      field: 'cc',
      reason: 'cc_not_reverse_alias',
      fix: 'use_reverse_alias',
    }
    const kinds = (l: Leak, ids: Identity[]) => leakFixes(l, reply, ids).map((f) => f.kind)
    expect(kinds(leak, [])).toEqual(['reverse', 'remove'])
    expect(kinds({ ...leak, fix: 'remove_recipient' }, [])).toEqual(['remove'])
    const direct = { ...leak, fix: 'use_alias_identity', reason: 'real_address_from' }
    expect(kinds(direct, [])).toEqual(['remove'])
    const shop = identity('i2', 'SHOP@sl.example.org')
    expect(kinds(direct, [identity('i1', 'ada@example.com'), shop])).toEqual(['identity', 'remove'])
    expect(leakFixes(direct, reply, [shop])[0]).toEqual({ kind: 'identity', identity: shop })
    expect(aliasIdentity(reply, [{ ...shop, active: false }])).toBeUndefined()
    const linked = identity('i3', 'masked@example.net', { alias: 'shop@sl.example.org' })
    expect(aliasIdentity(reply, [linked])).toBe(linked)
    expect(aliasIdentity(route, [shop])).toBeUndefined()
  })
})
