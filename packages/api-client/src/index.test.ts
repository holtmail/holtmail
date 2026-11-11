import { afterEach, describe, expect, mock, test } from 'bun:test'

import {
  ApiError,
  applyItip,
  bodyUrl,
  calendarExportUrl,
  calendarParams,
  client,
  contactsExportUrl,
  deviceProfileLink,
  exportUrl,
  fetchBody,
  idempotencyKey,
  isUnauthorized,
  itemVersionRawUrl,
  login,
  logout,
  peopleParams,
  rawUrl,
  refusedLeaks,
  uploadDraftAttachment,
} from './index.ts'

const realFetch = globalThis.fetch

afterEach(() => {
  globalThis.fetch = realFetch
})

function stubFetch(response: Response) {
  const stub = mock(async (_input: RequestInfo | URL, _init?: RequestInit) => response)
  globalThis.fetch = Object.assign(stub, { preconnect: realFetch.preconnect })
  return stub
}

describe('urls', () => {
  test('body and raw urls are same-origin and escape the id', () => {
    expect(bodyUrl('abc')).toBe('/api/messages/abc/body')
    expect(bodyUrl('abc', 'block')).toBe('/api/messages/abc/body')
    expect(bodyUrl('abc', 'proxy')).toBe('/api/messages/abc/body?remote=proxy')
    expect(bodyUrl('abc', 'direct')).toBe('/api/messages/abc/body?remote=direct')
    expect(bodyUrl('abc', 'proxy', false)).toBe('/api/messages/abc/body?remote=proxy&saved=false')
    expect(rawUrl('a/b')).toBe('/api/messages/a%2Fb/raw')
    expect(itemVersionRawUrl('i/1', 3)).toBe('/api/items/i%2F1/versions/3/raw')
  })

  test('builds export addresses with one scope', () => {
    expect(exportUrl('a1', 'mbox')).toBe('/api/archives/a1/export?format=mbox')
    expect(exportUrl('a1', 'zip', { folder: 'f1' })).toBe(
      '/api/archives/a1/export?format=zip&folder=f1',
    )
    expect(exportUrl('a1', 'zip', { label: 'News & updates' })).toBe(
      '/api/archives/a1/export?format=zip&label=News+%26+updates',
    )
    expect(exportUrl('a1', 'mbox', { q: ' from:ada ' })).toBe(
      '/api/archives/a1/export?format=mbox&q=from%3Aada',
    )
    expect(exportUrl('a1', 'mbox', { q: '   ' })).toBe('/api/archives/a1/export?format=mbox')
    expect(exportUrl('a1', 'zip', { folder: 'f1', q: 'x' })).toBe(
      '/api/archives/a1/export?format=zip&folder=f1',
    )
  })

  test('builds contacts and calendar export addresses', () => {
    expect(contactsExportUrl('a1', 'vcf')).toBe('/api/archives/a1/contacts/export?format=vcf')
    expect(contactsExportUrl('a1', 'zip', { collection: 'c1', removed: true })).toBe(
      '/api/archives/a1/contacts/export?format=zip&collection=c1&removed=true',
    )
    expect(contactsExportUrl('a1', 'vcf', { collection: 'c1', person: 'p1' })).toBe(
      '/api/archives/a1/contacts/export?format=vcf&person=p1',
    )
    expect(calendarExportUrl('a1', 'ics', { from: 10.5, to: 20.5 })).toBe(
      '/api/archives/a1/calendar/export?format=ics&from=10&to=21',
    )
    expect(calendarExportUrl('a1', 'zip', { collection: 'k1', from: 20, to: 10 })).toBe(
      '/api/archives/a1/calendar/export?format=zip&collection=k1',
    )
  })
})

describe('fetchBody', () => {
  test('reads the html and the remote-blocked header', async () => {
    const stub = stubFetch(
      new Response('<p>hi</p>', {
        headers: { 'x-holtmail-remote-blocked': '1', 'x-holtmail-remote-saved': '1' },
      }),
    )
    expect(await fetchBody('m1')).toEqual({
      html: '<p>hi</p>',
      remoteBlocked: true,
      savedShown: true,
    })
    expect(stub.mock.calls[0]?.[0]).toBe('/api/messages/m1/body')
  })

  test('surfaces the API error message', async () => {
    stubFetch(Response.json({ error: 'message not found' }, { status: 404 }))
    const error = await fetchBody('missing').catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect(error).toMatchObject({ status: 404, message: 'message not found' })
  })
})

describe('body-less posts', () => {
  test('a profile link and apply anyway send json, as the CSRF guard asks', async () => {
    const realPost = client.POST
    const post = mock(async (_path: string, _init?: { headers?: HeadersInit }) => ({
      data: {},
      response: new Response('{}'),
    }))
    client.POST = post as unknown as typeof client.POST
    try {
      await deviceProfileLink('d1')
      await applyItip('m1')
    } finally {
      client.POST = realPost
    }
    expect(post.mock.calls.map((call) => call[0])).toEqual([
      '/api/devices/{id}/profile',
      '/api/messages/{id}/itip/apply',
    ])
    for (const call of post.mock.calls) {
      expect(new Headers(call[1]?.headers).get('content-type')).toBe('application/json')
    }
  })
})

describe('sign-in', () => {
  test('login posts json and returns the user', async () => {
    const user = { id: 'u1', email: 'ada@example.com', name: 'Ada', role: 'owner' }
    const stub = stubFetch(Response.json({ user }))
    expect(await login({ email: 'ada@example.com', password: 'secret' })).toEqual(user)
    const [url, init] = stub.mock.calls[0] ?? []
    expect(url).toBe('/api/login')
    expect(init?.method).toBe('POST')
    expect(new Headers(init?.headers).get('content-type')).toBe('application/json')
    expect(init?.body).toBe(JSON.stringify({ email: 'ada@example.com', password: 'secret' }))
  })

  test('logout posts an empty json object', async () => {
    const stub = stubFetch(new Response(null, { status: 204 }))
    await logout()
    const [url, init] = stub.mock.calls[0] ?? []
    expect(url).toBe('/api/logout')
    expect(init?.body).toBe('{}')
    expect(new Headers(init?.headers).get('content-type')).toBe('application/json')
  })

  test('a rate limit carries Retry-After', async () => {
    stubFetch(
      Response.json(
        { error: 'too many attempts' },
        { status: 429, headers: { 'Retry-After': '600' } },
      ),
    )
    const error = await login({ email: 'a@example.com', password: 'x' }).catch((e: unknown) => e)
    expect(error).toMatchObject({ status: 429, retryAfter: 600 })
    expect(isUnauthorized(error)).toBe(false)
    expect(isUnauthorized(new ApiError(401, 'sign in required'))).toBe(true)
  })
})

describe('people', () => {
  test('leaves empty filters out of the query', () => {
    expect(peopleParams({})).toEqual({})
    expect(peopleParams({ q: '  ', source: '', differences: false })).toEqual({})
    expect(peopleParams({ q: ' ada ', source: 'mail', differences: true })).toEqual({
      q: 'ada',
      source: 'mail',
      has_differences: true,
    })
  })
})

describe('calendar', () => {
  test('joins calendars, rounds the span and leaves defaults out', () => {
    expect(calendarParams({ from: 10.4, to: 20.2 })).toEqual({ from: 10, to: 21 })
    expect(calendarParams({ from: 1, to: 2, collections: ['b', 'a'], showRemoved: true })).toEqual({
      from: 1,
      to: 2,
      collections: 'a,b',
      show_removed: true,
    })
    expect(calendarParams({ from: 1, to: 2, collections: [] })).toEqual({
      from: 1,
      to: 2,
      collections: '',
    })
  })
})

describe('drafts', () => {
  test('uploads an attachment as octet-stream with its name and type', async () => {
    const draft = {
      id: 'd1',
      archive: 'a1',
      version: 3,
      payload: {},
      saved_at: 0,
      created_at: 0,
      leaks: [],
    }
    const stub = stubFetch(Response.json(draft))
    const file = new File(['hi'], 'Plans for the café.pdf', { type: 'application/pdf' })
    expect(await uploadDraftAttachment('d/1', file)).toEqual(draft)
    const [url, init] = stub.mock.calls[0] ?? []
    expect(url).toBe(
      '/api/drafts/d%2F1/attachments?name=Plans+for+the+caf%C3%A9.pdf&content_type=application%2Fpdf',
    )
    expect(init?.method).toBe('POST')
    expect(new Headers(init?.headers).get('content-type')).toBe('application/octet-stream')
  })

  test('a refused upload throws the server message', async () => {
    stubFetch(Response.json({ error: 'attachments are at most 50 MB' }, { status: 413 }))
    const error = await uploadDraftAttachment('d1', new File(['x'], 'big.bin')).catch(
      (e: unknown) => e,
    )
    expect(error).toBeInstanceOf(ApiError)
    expect(error).toMatchObject({ status: 413, message: 'attachments are at most 50 MB' })
  })

  test('idempotency keys are fresh each time', () => {
    expect(idempotencyKey()).not.toBe(idempotencyKey())
    expect(idempotencyKey()).toMatch(/^[0-9a-f-]{36}$/)
  })

  test('idempotency keys work without crypto.randomUUID (plain HTTP)', () => {
    const own = Object.getOwnPropertyDescriptor(crypto, 'randomUUID')
    Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true })
    try {
      const key = idempotencyKey()
      expect(key).toMatch(/^[0-9a-f]{32}$/)
      expect(idempotencyKey()).not.toBe(key)
    } finally {
      if (own) Object.defineProperty(crypto, 'randomUUID', own)
      else Reflect.deleteProperty(crypto, 'randomUUID')
    }
  })
})

describe('refusedLeaks', () => {
  const leak = {
    recipient: 'carol@example.org',
    field: 'cc',
    reason: 'cc_not_reverse_alias',
    fix: 'remove_recipient',
  }

  test('reads the leaks of a refused send', () => {
    const error = new ApiError(409, 'leaks', undefined, { error: 'leaks', leaks: [leak] })
    expect(refusedLeaks(error)).toEqual([leak])
  })

  test('is null for other errors', () => {
    expect(refusedLeaks(new ApiError(409, 'saved again', undefined, { error: 'x' }))).toBeNull()
    expect(refusedLeaks(new ApiError(400, 'bad', undefined, { leaks: [leak] }))).toBeNull()
    expect(
      refusedLeaks(new ApiError(409, 'odd', undefined, { leaks: [{ recipient: 1 }] })),
    ).toBeNull()
    expect(refusedLeaks(new Error('plain'))).toBeNull()
  })
})
