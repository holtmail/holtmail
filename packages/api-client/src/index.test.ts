import { afterEach, describe, expect, mock, test } from 'bun:test'

import { ApiError, bodyUrl, fetchBody, isUnauthorized, login, logout, rawUrl } from './index.ts'

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
    expect(rawUrl('a/b')).toBe('/api/messages/a%2Fb/raw')
  })
})

describe('fetchBody', () => {
  test('reads the html and the remote-blocked header', async () => {
    const stub = stubFetch(
      new Response('<p>hi</p>', { headers: { 'x-holtmail-remote-blocked': '1' } }),
    )
    expect(await fetchBody('m1')).toEqual({ html: '<p>hi</p>', remoteBlocked: true })
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
