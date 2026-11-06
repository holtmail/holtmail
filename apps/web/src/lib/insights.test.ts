import { describe, expect, test } from 'bun:test'

import type {
  CodeInsight,
  Insight,
  MessageInsights,
  UnsubscribeInsight,
} from '@holtmail/api-client'

import {
  cardView,
  codeAge,
  codeTarget,
  codeTime,
  copyValue,
  groupCode,
  isStale,
  orderInsights,
  reasonText,
  STALE_CODE_MS,
  type TargetMessage,
  unsubscribeTarget,
} from './insights.ts'

const code = (value: string, extra: Partial<CodeInsight> = {}): CodeInsight => ({
  type: 'code',
  value,
  confidence: 'likely',
  score: 500,
  forwarded: false,
  ...extra,
})

const unsubscribeItem = (): UnsubscribeInsight => ({
  type: 'unsubscribe',
  https: 'https://news.example.com/leave',
  one_click: false,
  confidence: 'certain',
  score: 900,
  forwarded: false,
})

const insights = (items: Insight[], extra: Partial<MessageInsights> = {}): MessageInsights => ({
  status: 'ready',
  own: false,
  trust: { state: 'unknown' },
  items,
  unsubscribe: null,
  ...extra,
})

const minute = 60_000
const now = Date.UTC(2026, 10, 6, 12, 0)

describe('orderInsights', () => {
  test('puts codes first, by confidence then score, keeping ties stable', () => {
    const ordered = orderInsights([
      unsubscribeItem(),
      code('111111', { score: 900 }),
      code('222222', { confidence: 'certain', score: 100 }),
      code('333333', { score: 900 }),
    ])
    expect(ordered.map((item) => (item.type === 'code' ? item.value : item.type))).toEqual([
      '222222',
      '111111',
      '333333',
    ])
  })
})

describe('groupCode', () => {
  test.each([
    ['1234', '1234'],
    ['12345', '12345'],
    ['482913', '482 913'],
    ['1234567', '1234567'],
    ['12345678', '1234 5678'],
    ['AB12CD', 'AB12CD'],
    ['482 913', '482 913'],
  ])('%s shows as %s', (value, shown) => {
    expect(groupCode(value)).toBe(shown)
  })

  test('copies without spaces', () => {
    expect(copyValue('482 913')).toBe('482913')
    expect(copyValue(groupCode('12345678'))).toBe('12345678')
  })
})

describe('freshness', () => {
  test('uses the earlier of received and sent', () => {
    expect(codeTime({ received_at: now, sent_at: now - 5 * minute })).toBe(now - 5 * minute)
    expect(codeTime({ received_at: now - 5 * minute, sent_at: now })).toBe(now - 5 * minute)
  })

  test('uses whichever time exists', () => {
    expect(codeTime({ received_at: now })).toBe(now)
    expect(codeTime({ sent_at: now, received_at: null })).toBe(now)
  })

  test('is stale with no time known', () => {
    expect(codeTime({})).toBeUndefined()
    expect(isStale(undefined, now)).toBe(true)
  })

  test('turns stale after 15 minutes', () => {
    expect(isStale(now - STALE_CODE_MS, now)).toBe(false)
    expect(isStale(now - STALE_CODE_MS - 1, now)).toBe(true)
  })

  test('says how old a collapsed code is', () => {
    expect(codeAge(now - 3 * 24 * 60 * minute, now)).toBe('Code from 3 days ago')
    expect(codeAge(now - 20 * minute, now)).toBe('Code from 20 minutes ago')
    expect(codeAge(undefined, now)).toBe('Code of unknown age')
  })
})

describe('reasonText', () => {
  test('explains each reason', () => {
    expect(reasonText('members')).toBe('Only owners and admins can unsubscribe.')
    expect(reasonText('disabled')).toBe('One-click unsubscribe is turned off on this server.')
    expect(reasonText(null)).toBeUndefined()
  })
})

describe('cardView', () => {
  test('shows nothing for statuses without items or with nothing visible', () => {
    expect(cardView(undefined)).toBeUndefined()
    expect(cardView(insights([code('482913')], { status: 'pending' }))).toBeUndefined()
    expect(cardView(insights([code('482913')], { status: 'unreadable' }))).toBeUndefined()
    expect(cardView(insights([]))).toBeUndefined()
  })

  test('shows partial and outdated like ready', () => {
    expect(cardView(insights([code('482913')], { status: 'outdated' }))?.codes).toHaveLength(1)
    expect(cardView(insights([code('482913')], { status: 'partial' }))?.codes).toHaveLength(1)
  })

  test('prefers the https link and carries the reason', () => {
    const view = cardView(
      insights([unsubscribeItem()], {
        trust: { state: 'aligned', domain: 'example.com' },
        unsubscribe: {
          https: 'https://news.example.com/leave',
          mailto: 'mailto:leave@example.com',
          one_click: { available: false, reason: 'disabled' },
        },
      }),
    )
    expect(view?.unsubscribe).toEqual({ href: 'https://news.example.com/leave', kind: 'https' })
    expect(view?.reason).toBe(reasonText('disabled'))
    expect(view?.unchecked).toBe(false)
  })

  test('falls back to mailto and has no link for members', () => {
    const mail = cardView(
      insights([], {
        unsubscribe: { mailto: 'mailto:leave@example.com', one_click: { available: false } },
      }),
    )
    expect(mail?.unsubscribe?.kind).toBe('mailto')
    const member = cardView(
      insights([], { unsubscribe: { one_click: { available: false, reason: 'members' } } }),
    )
    expect(member).toBeUndefined()
  })
})

const message = (id: string, minutesAgo: number, items: Insight[]): TargetMessage => ({
  id,
  received_at: now - minutesAgo * minute,
  insights: insights(items, {
    unsubscribe: items.some((item) => item.type === 'unsubscribe')
      ? { https: 'https://news.example.com/leave', one_click: { available: false } }
      : null,
  }),
})

const all = () => true

describe('targets', () => {
  const thread = [
    message('a', 600, [code('111111')]),
    message('b', 5, [code('222222')]),
    message('c', 1, [unsubscribeItem()]),
    message('d', 400, [code('333333')]),
  ]

  test('picks the fresh code in a thread of stale ones', () => {
    expect(codeTarget(thread, all, undefined, now)?.id).toBe('b')
  })

  test('picks the newest code when every one is stale', () => {
    expect(codeTarget(thread, all, undefined, now + 60 * minute)?.id).toBe('d')
  })

  test('only looks at expanded messages', () => {
    expect(codeTarget(thread, (id) => id !== 'b', undefined, now)?.id).toBe('d')
  })

  test('the focused message wins, even with nothing to copy', () => {
    expect(codeTarget(thread, all, 'a', now)?.id).toBe('a')
    expect(codeTarget(thread, all, 'c', now)).toBeUndefined()
  })

  test('picks the newest message with an unsubscribe link', () => {
    expect(unsubscribeTarget(thread, all, undefined)?.id).toBe('c')
    expect(unsubscribeTarget(thread, all, 'a')).toBeUndefined()
  })
})
