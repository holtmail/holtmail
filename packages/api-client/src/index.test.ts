import { afterEach, describe, expect, mock, test } from 'bun:test'

import {
  ApiError,
  bodyUrl,
  calendarExportUrl,
  calendarParams,
  contactsExportUrl,
  exportUrl,
  fetchBody,
  isUnauthorized,
  itemVersionRawUrl,
  login,
  logout,
  peopleParams,
  rawUrl,
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
