import { describe, expect, test } from 'bun:test'

import {
  ApiError,
  type MessageInsights,
  type Unsubscribe,
  type UnsubscribeOutcome,
} from '@holtmail/api-client'

import { cardView, unsubscribeKey, unsubscribeTarget } from './insights.ts'
import {
  linkHost,
  oneClickOffered,
  outcomeNote,
  requestedOnText,
  unsubscribeDomain,
  unsubscribeErrorText,
} from './unsubscribe.ts'

const insights = (unsubscribe: Unsubscribe | null, domain?: string): MessageInsights => ({
  status: 'ready',
  own: false,
  trust: domain ? { state: 'aligned', domain } : { state: 'unknown' },
  kind: null,
  topics: [],
  items: [],
  unsubscribe,
})

const offered: Unsubscribe = {
  https: 'https://news.example.com/leave?u=1',
  one_click: { available: true },
}
const linkOnly: Unsubscribe = {
  https: 'https://news.example.com/leave',
  one_click: { available: false, reason: 'not_covered' },
}
const at = Date.UTC(2026, 10, 6, 12, 0)
const date = (time: number) => `<${time}>`
const all = () => true
const result = (outcome: UnsubscribeOutcome, repeated = false, status?: number) => ({
  repeated,
  request: { outcome, requested_at: at, status },
})

describe('linkHost', () => {
  test('drops port, user name and trailing dot, and lowercases', () => {
    expect(linkHost('https://user:pw@News.Example.COM.:8443/leave')).toBe('news.example.com')
  })

  test('keeps an A-label as it is', () => {
    expect(linkHost('https://xn--bcher-kva.example/leave')).toBe('xn--bcher-kva.example')
  })

  test('gives nothing for a missing or broken link', () => {
    expect(linkHost(undefined)).toBeUndefined()
    expect(linkHost('not a url')).toBeUndefined()
  })
})

describe('unsubscribeDomain', () => {
  test('names the host of the https link first', () => {
    expect(unsubscribeDomain(insights(offered, 'example.org'), 'ada@example.net')).toBe(
      'news.example.com',
    )
  })

  test('falls back to the aligned domain, then the sender', () => {
    const mailto: Unsubscribe = {
      mailto: 'mailto:leave@example.com',
      one_click: { available: false },
    }
    expect(unsubscribeDomain(insights(mailto, 'Example.org'), 'ada@example.net')).toBe(
      'example.org',
    )
    expect(unsubscribeDomain(insights(mailto), 'ada@Example.NET')).toBe('example.net')
    expect(unsubscribeDomain(insights(mailto))).toBeUndefined()
  })
})

describe('oneClickOffered', () => {
  test('only when the server says available', () => {
    expect(oneClickOffered(insights(offered))).toBe(true)
    expect(oneClickOffered(insights(linkOnly))).toBe(false)
    expect(oneClickOffered(insights(null))).toBe(false)
    expect(oneClickOffered(undefined)).toBe(false)
  })
})

describe('outcomeNote', () => {
  test('accepted is a success with no link', () => {
    expect(outcomeNote(result('accepted', false, 200), 'example.com', date)).toEqual({
      text: 'Requested. example.com accepted the request.',
      tone: 'success',
      offerLink: false,
    })
  })

  test('redirected offers the link', () => {
    const note = outcomeNote(result('redirected', false, 302), 'example.com', date)
    expect(note.text).toBe('example.com wants you to finish on their site.')
    expect(note.tone).toBe('warning')
    expect(note.offerLink).toBe(true)
  })

  test('failures say what went wrong and offer the link', () => {
    expect(outcomeNote(result('refused'), 'example.com', date).text).toContain(
      'did not contact example.com',
    )
    expect(outcomeNote(result('http_error', false, 503), 'example.com', date).text).toBe(
      'example.com answered with an error (HTTP 503). Try their unsubscribe page instead.',
    )
    expect(outcomeNote(result('http_error'), 'example.com', date).text).toStartWith(
      'example.com answered with an error. ',
    )
    const network = outcomeNote(result('network'), 'example.com', date)
    expect(network.text).toBe(
      'Holtmail could not reach example.com. Try their unsubscribe page instead.',
    )
    expect(network.offerLink).toBe(true)
  })

  test('a repeated answer names the earlier request', () => {
    expect(outcomeNote(result('accepted', true), 'example.com', date).text).toBe(
      `Already requested on <${at}>. Requested. example.com accepted the request.`,
    )
  })
})

describe('unsubscribeErrorText', () => {
  test('a 400 shows the reason', () => {
    const error = new ApiError(400, 'one-click unsubscribe is not available: recently_requested')
    expect(unsubscribeErrorText(error)).toBe('Unsubscribing was requested recently.')
    expect(
      unsubscribeErrorText(new ApiError(400, 'one-click unsubscribe is not available: not_https')),
    ).toBe('The one-click link is not secure (no HTTPS).')
  })

  test('a 429 asks to wait a minute', () => {
    expect(unsubscribeErrorText(new ApiError(429, 'Too Many Requests', 60))).toBe(
      'The last request failed a moment ago. Try again in a minute.',
    )
  })

  test('anything else is a generic failure', () => {
    expect(unsubscribeErrorText(new ApiError(400, 'confirm with {"confirm": true}'))).toBe(
      'Could not send the request. Try again later.',
    )
    expect(unsubscribeErrorText(new ApiError(500, 'boom'))).toBe(
      'Could not send the request. Try again later.',
    )
    expect(unsubscribeErrorText(new Error('offline'))).toBe(
      'Could not send the request. Try again later.',
    )
  })
})

describe('requestedOnText', () => {
  test('just the date when accepted', () => {
    expect(requestedOnText({ outcome: 'accepted', requested_at: at, status: 200 }, date)).toBe(
      `Unsubscribe requested on <${at}>`,
    )
  })

  test('adds the outcome otherwise', () => {
    expect(requestedOnText({ outcome: 'redirected', requested_at: at, status: 302 }, date)).toBe(
      `Unsubscribe requested on <${at}> · finish on their site`,
    )
    expect(requestedOnText({ outcome: 'http_error', requested_at: at, status: 500 }, date)).toBe(
      `Unsubscribe requested on <${at}> · error (HTTP 500)`,
    )
    expect(requestedOnText({ outcome: 'network', requested_at: at }, date)).toBe(
      `Unsubscribe requested on <${at}> · could not reach the sender`,
    )
  })

  test('formats in the viewer locale by default', () => {
    expect(requestedOnText({ outcome: 'accepted', requested_at: at })).toContain('2026')
  })
})

describe('Shift+U', () => {
  test('opens the dialog when one-click is offered, else the link', () => {
    expect(unsubscribeKey(cardView(insights(offered)))).toBe('dialog')
    expect(unsubscribeKey(cardView(insights(linkOnly)))).toBe('link')
    expect(unsubscribeKey(cardView(insights(null)))).toBeUndefined()
  })

  const thread = [
    { id: 'a', insights: insights(offered) },
    { id: 'b', insights: insights(linkOnly) },
    { id: 'c', insights: insights(null) },
  ]

  test('picks the newest expanded message with either', () => {
    expect(unsubscribeTarget(thread, all, undefined)?.id).toBe('b')
    expect(unsubscribeTarget(thread, (id) => id !== 'b', undefined)?.id).toBe('a')
  })

  test('the focused message wins, even with nothing to do', () => {
    expect(unsubscribeTarget(thread, all, 'a')?.id).toBe('a')
    expect(unsubscribeTarget(thread, all, 'c')).toBeUndefined()
  })
})
