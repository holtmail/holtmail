import { describe, expect, test } from 'bun:test'

import type { Identity, Transport } from '@holtmail/api-client'

import {
  allowedFromLines,
  appendState,
  accountServer,
  emptyTransportForm,
  identityForm,
  identityRequest,
  identityUpdate,
  newIdentityForm,
  parseLimit,
  parseSentCopy,
  sentCopyAccount,
  sentCopyValue,
  transportFields,
  transportForm,
  transportRequest,
  transportUpdate,
  validPattern,
} from './sending.ts'

const transport: Transport = {
  id: 't1',
  archive: 'a1',
  name: 'Fastmail SMTP',
  kind: 'smtp',
  host: 'smtp.example.com',
  port: 587,
  security: 'starttls',
  username: 'ada',
  has_password: true,
  allowed_from: ['ada@example.com', '*@example.org'],
  limits: { max_size: 25 * 1024 * 1024, max_recipients: null, per_hour: 100 },
  enabled: true,
  created_at: 0,
  grants: [],
}

describe('transport form', () => {
  test('patterns', () => {
    for (const p of [
      'ada@example.com',
      '*@example.org',
      '*@*.example.org',
      'ada+x@a.example.net',
    ]) {
      expect(validPattern(p)).toBe(true)
    }
    for (const p of [
      'example.org',
      '@example.org',
      'a*@example.org',
      '*@*',
      '*@org',
      'a b@x.org',
    ]) {
      expect(validPattern(p)).toBe(false)
    }
    expect(allowedFromLines(' ada@example.com\n\n*@example.org, bob@example.net ')).toEqual([
      'ada@example.com',
      '*@example.org',
      'bob@example.net',
    ])
  })

  test('limits', () => {
    expect(parseLimit('')).toBeNull()
    expect(parseLimit(' 25 ')).toBe(25)
    expect(parseLimit('0')).toBeUndefined()
    expect(parseLimit('2.5')).toBeUndefined()
  })

  test('round trips a transport and reports the first wrong field', () => {
    const result = transportRequest(transportForm(transport))
    expect('request' in result && result.request).toMatchObject({
      name: 'Fastmail SMTP',
      kind: 'smtp',
      host: 'smtp.example.com',
      port: 587,
      password: null,
      allowed_from: ['ada@example.com', '*@example.org'],
      limits: { max_size: 25 * 1024 * 1024, max_recipients: null, per_hour: 100 },
    })
    expect(transportRequest(emptyTransportForm())).toEqual({ error: { field: 'name' } })
    expect(transportRequest({ ...emptyTransportForm(), name: 'x' })).toEqual({
      error: { field: 'host' },
    })
    expect(transportRequest({ ...emptyTransportForm('account'), name: 'x' })).toEqual({
      error: { field: 'account' },
    })
    expect(
      transportRequest({ ...transportForm(transport), allowedFrom: 'ada@example.com\nnope' }),
    ).toEqual({ error: { field: 'allowedFrom', pattern: 'nope' } })
    expect(transportRequest({ ...transportForm(transport), perHour: '-1' })).toEqual({
      error: { field: 'limits' },
    })
  })
})

describe('transport kinds', () => {
  const jmap: Transport = {
    ...transport,
    id: 't2',
    name: 'Stalwart JMAP',
    kind: 'jmap',
    host: null,
    port: null,
    security: null,
    url: 'https://jmap.example.com/.well-known/jmap',
    auth: 'password',
    username: 'ada',
  }

  test('an account transport speaks what its account speaks', () => {
    expect(accountServer('imap')).toBe('smtp')
    expect(accountServer(undefined)).toBe('smtp')
    expect(accountServer('jmap')).toBe('jmap')
    expect(accountServer('graph')).toBe('graph')
    const account = { ...emptyTransportForm('account'), name: 'Own', account: 'acc1' }
    expect(transportFields(account, 'imap')).toMatchObject({ server: true, password: true })
    expect(transportFields(account, 'graph')).toEqual({
      server: false,
      url: false,
      auth: false,
      mailbox: false,
      username: false,
      password: false,
    })
    const result = transportRequest({ ...account, host: 'ignored.example.com' }, 'jmap')
    expect('request' in result && result.request).toMatchObject({
      kind: 'account',
      account: 'acc1',
      host: null,
      port: null,
      username: null,
      password: null,
      url: null,
    })
  })

  test('a new JMAP transport needs its address, a user name for passwords and the secret', () => {
    const form = { ...emptyTransportForm('jmap'), name: 'JMAP' }
    expect(transportRequest(form)).toEqual({ error: { field: 'url' } })
    const withUrl = { ...form, url: ' https://jmap.example.com ' }
    expect(transportRequest(withUrl)).toEqual({ error: { field: 'username' } })
    expect(transportRequest({ ...withUrl, username: 'ada' })).toEqual({
      error: { field: 'password' },
    })
    const token = transportRequest({ ...withUrl, auth: 'token', password: 'tok' })
    expect('request' in token && token.request).toMatchObject({
      kind: 'jmap',
      url: 'https://jmap.example.com',
      auth: 'token',
      username: null,
      password: 'tok',
      host: null,
      security: null,
      mailbox: null,
    })
  })

  test('a Graph transport needs only a token; the endpoint and mailbox are optional', () => {
    const form = { ...emptyTransportForm('graph'), name: 'Graph' }
    expect(transportFields(form)).toMatchObject({ url: true, mailbox: true, username: false })
    expect(transportRequest(form)).toEqual({ error: { field: 'password' } })
    const result = transportRequest({ ...form, password: 'tok', mailbox: ' ada@example.com ' })
    expect('request' in result && result.request).toMatchObject({
      kind: 'graph',
      url: null,
      auth: null,
      mailbox: 'ada@example.com',
      password: 'tok',
    })
  })

  test('editing a JMAP transport sends its own fields, the address only when it changed', () => {
    const form = transportForm(jmap)
    expect(form).toMatchObject({ kind: 'jmap', url: jmap.url, auth: 'password', password: '' })
    const same = transportUpdate(form, jmap)
    expect('request' in same && same.request).toEqual({
      name: 'Stalwart JMAP',
      allowed_from: ['ada@example.com', '*@example.org'],
      limits: { max_size: 25 * 1024 * 1024, max_recipients: null, per_hour: 100 },
      enabled: true,
      auth: 'password',
      username: 'ada',
    })
    expect(transportUpdate({ ...form, url: 'https://other.example.com' }, jmap)).toEqual({
      error: { field: 'secretAgain' },
    })
    expect(transportUpdate({ ...form, auth: 'token' }, jmap)).toEqual({
      error: { field: 'secretForAuth' },
    })
    const switched = transportUpdate({ ...form, auth: 'token', password: 'tok' }, jmap)
    expect('request' in switched && switched.request).toMatchObject({
      auth: 'token',
      password: 'tok',
    })
    const moved = transportUpdate(
      { ...form, url: 'https://other.example.com', auth: 'token', password: 'tok' },
      jmap,
    )
    expect('request' in moved && moved.request).toMatchObject({
      url: 'https://other.example.com',
      auth: 'token',
      username: '',
      password: 'tok',
    })
    expect('request' in moved && 'host' in moved.request).toBe(false)
  })

  test('editing an SMTP transport keeps sending its server fields', () => {
    const result = transportUpdate(transportForm(transport), transport)
    expect('request' in result && result.request).toMatchObject({
      host: 'smtp.example.com',
      port: 587,
      security: 'starttls',
      username: 'ada',
    })
    expect('request' in result && 'password' in result.request).toBe(false)
  })
})

const capabilities = (on: string[], active: string[]) => ({
  account: 'acc1',
  capabilities: on,
  active,
  switches: { enabled: true, send: true, rules: true },
})

describe('sent copy', () => {
  test('the account is the identity’s, else its account transport’s', () => {
    const own = { ...transport, id: 't3', kind: 'account', account: 'acc2' }
    expect(sentCopyAccount({ account: 'acc1', transport: 't3' }, [own])).toBe('acc1')
    expect(sentCopyAccount({ account: null, transport: 't3' }, [own])).toBe('acc2')
    expect(sentCopyAccount({ account: null, transport: 't1' }, [transport, own])).toBeNull()
  })

  test('append is available once mail.append is in effect', () => {
    expect(appendState(null, undefined)).toBe('noAccount')
    expect(appendState('acc1', undefined)).toBe('unknown')
    expect(appendState('acc1', capabilities([], []))).toBe('off')
    expect(appendState('acc1', capabilities(['mail.append'], []))).toBe('switchedOff')
    expect(appendState('acc1', capabilities(['mail.append'], ['mail.append']))).toBe('available')
  })
})

describe('identity form', () => {
  test('sent copy policies', () => {
    expect(parseSentCopy('append:Sent Items')).toEqual({ choice: 'append', folder: 'Sent Items' })
    expect(parseSentCopy('provider')).toEqual({ choice: 'provider', folder: '' })
    expect(parseSentCopy('none').choice).toBe('none')
    expect(sentCopyValue('append', ' ')).toBe('append:Sent')
    expect(sentCopyValue('provider', 'x')).toBe('provider')
  })

  test('a suggestion fills a new identity', () => {
    const form = newIdentityForm([transport], 'u1', {
      address: 'ada@example.org',
      source: 'account',
      account: 'acc1',
      alias: null,
      transport: 't1',
    })
    expect(form).toMatchObject({
      address: 'ada@example.org',
      transport: 't1',
      account: 'acc1',
      sentCopy: 'none',
      users: ['u1'],
    })
    expect(newIdentityForm([{ ...transport, kind: 'account' }], undefined).sentCopy).toBe(
      'provider',
    )
  })

  test('requests clear optional fields the way the server expects', () => {
    const identity = {
      id: 'i1',
      archive: 'a1',
      address: 'ada@example.com',
      name: 'Ada',
      signature_html: '<p>Ada</p>',
      signature_text: null,
      reply_to: null,
      transport: 't1',
      sent_copy: 'append:Sent',
      bcc_self: false,
      account: null,
      alias: null,
      plus_addresses: true,
      active: true,
      created_at: 0,
      users: ['u1'],
      default: true,
    } as Identity
    const form = { ...identityForm(identity), name: ' ', signatureHtml: '<p></p>' }
    expect(identityRequest(form)).toMatchObject({
      name: null,
      signature_html: null,
      sent_copy: 'append:Sent',
      plus_addresses: true,
    })
    expect(identityUpdate(form)).toMatchObject({ name: '', signature_html: '', reply_to: '' })
    expect('account' in identityUpdate(form)).toBe(false)
  })
})
