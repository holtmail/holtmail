import { describe, expect, test } from 'bun:test'

import type { CategoryOverride, CategoryTopic, MessageInsights } from '@holtmail/api-client'

import {
  applyOverride,
  canCategorize,
  cardChips,
  categoryLabel,
  categoryTarget,
  heldBackText,
  initialDraft,
  kindLabel,
  listKindGlyph,
  listTopic,
  overrideRequest,
  overrideSender,
  overrideSummary,
  paletteEntries,
  parseCategorySearch,
  scopeKey,
  scopeText,
  senderOverrides,
  shownTopics,
  sidebarCounts,
  topicLabel,
  trustedIdError,
  trustNote,
} from './categories.ts'

const topic = (id: string, extra: Partial<CategoryTopic> = {}): CategoryTopic => ({
  id,
  source: 'domain',
  overridden: false,
  ...extra,
})

const insights = (extra: Partial<MessageInsights> = {}): MessageInsights => ({
  status: 'ready',
  own: false,
  trust: { state: 'unknown' },
  kind: { id: 'bulk', source: 'headers', overridden: false },
  topics: [],
  items: [],
  unsubscribe: null,
  ...extra,
})

const override = (extra: Partial<CategoryOverride>): CategoryOverride => ({
  id: 'o1',
  scope: 'address',
  key: 'ada@example.com',
  kind: null,
  topics: null,
  created_at: 0,
  created_by: 'u1',
  ...extra,
})

describe('labels', () => {
  test('known kinds and topics get their names', () => {
    expect(kindLabel('bulk')).toBe('Newsletter')
    expect(topicLabel('travel')).toBe('Travel')
    expect(categoryLabel('notification')).toBe('Notification')
    expect(categoryLabel('purchases')).toBe('Purchases')
  })

  test('unknown ids fall back to the raw id', () => {
    expect(kindLabel('digest')).toBe('digest')
    expect(topicLabel('pets')).toBe('pets')
  })
})

describe('chips', () => {
  test('the card shows the kind and applied topics, never health or held-back ones', () => {
    const chips = cardChips(
      insights({
        topics: [
          topic('purchases'),
          topic('finance', { source: 'override', overridden: true, held_back: 'not_verified' }),
          topic('health'),
          topic('travel', { source: 'override', overridden: true }),
        ],
      }),
    )
    expect(chips.kind).toEqual({ id: 'bulk', label: 'Newsletter', overridden: false })
    expect(chips.topics.map((chip) => chip.id)).toEqual(['purchases', 'travel'])
    expect(chips.topics[1]?.overridden).toBe(true)
  })

  test('no insights means no chips', () => {
    expect(cardChips(null)).toEqual({ kind: undefined, topics: [] })
    expect(cardChips(insights({ kind: null })).kind).toBeUndefined()
  })

  test('list rows show one topic chip but never health', () => {
    expect(listTopic('travel')?.label).toBe('Travel')
    expect(listTopic('health')).toBeUndefined()
    expect(listTopic(null)).toBeUndefined()
  })

  test('list rows mark only bulk and notification with a glyph', () => {
    expect(listKindGlyph('bulk')).toBe('bulk')
    expect(listKindGlyph('notification')).toBe('notification')
    expect(listKindGlyph('personal')).toBeUndefined()
    expect(listKindGlyph(null)).toBeUndefined()
  })
})

describe('counts and palette', () => {
  test('the sidebar drops health and empty entries and keeps taxonomy order', () => {
    const counts = sidebarCounts({
      kinds: [
        { id: 'personal', count: 3 },
        { id: 'list', count: 0 },
        { id: 'bulk', count: 9 },
      ],
      topics: [
        { id: 'purchases', count: 2 },
        { id: 'health', count: 4 },
        { id: 'travel', count: 0 },
        { id: 'events', count: 1 },
      ],
    })
    expect(counts.kinds.map((entry) => entry.id)).toEqual(['personal', 'bulk'])
    expect(counts.topics.map((entry) => entry.id)).toEqual(['purchases', 'events'])
  })

  test('the palette offers every kind and topic but health', () => {
    const entries = paletteEntries()
    expect(entries.map((entry) => entry.id)).not.toContain('health')
    expect(entries.find((entry) => entry.id === 'travel')?.text).toBe('Show travel')
    expect(entries.find((entry) => entry.id === 'bulk')?.text).toBe('Show newsletter')
    expect(shownTopics).not.toContain('health')
  })

  test('the category search parameter accepts only known names', () => {
    expect(parseCategorySearch({ category: 'Travel ' })).toEqual({ category: 'travel' })
    expect(parseCategorySearch({ category: 'bulk' })).toEqual({ category: 'bulk' })
    expect(parseCategorySearch({ category: 'pets' })).toEqual({})
    expect(parseCategorySearch({ category: 3 })).toEqual({})
    expect(parseCategorySearch({})).toEqual({})
  })
})

describe('override scope', () => {
  const message = { from: [{ email: 'Ada@News.Example.com' }] }

  test('the sender is the only From address, lowercased', () => {
    expect(overrideSender(message)).toBe('ada@news.example.com')
    expect(overrideSender({ from: [] })).toBeUndefined()
    expect(overrideSender({ from: [{ email: 'a@example.com' }, { email: 'b@example.com' }] })).toBe(
      undefined,
    )
  })

  test('an alias hit uses the claimed sender', () => {
    expect(
      overrideSender({
        from: [{ email: 'relay@alias.example.org' }],
        alias: { sender: { email: 'grace@example.com' } },
      }),
    ).toBe('grace@example.com')
  })

  test('scope text names the address or everyone at the domain', () => {
    expect(scopeText('address', 'ada@example.com')).toBe('ada@example.com')
    expect(scopeText('domain', 'ada@example.com')).toBe('everyone at example.com')
  })

  test('a stored domain override covering a subdomain names its own key', () => {
    const existing = senderOverrides(
      [override({ id: 'd', scope: 'domain', key: 'example.com', kind: 'bulk' })],
      'ada@news.example.com',
    )
    expect(existing.domain?.id).toBe('d')
    expect(existing.address).toBeUndefined()
    expect(scopeKey('domain', 'ada@news.example.com', existing)).toBe('example.com')
    expect(scopeText('domain', 'ada@news.example.com', existing)).toBe('everyone at example.com')
    expect(scopeKey('domain', 'ada@news.example.com')).toBe('news.example.com')
  })

  test('a domain override does not reach a look-alike domain', () => {
    const existing = senderOverrides(
      [override({ scope: 'domain', key: 'example.com' })],
      'ada@badexample.com',
    )
    expect(existing.domain).toBeUndefined()
  })
})

describe('override request', () => {
  const initial = initialDraft(insights({ topics: [topic('purchases'), topic('health')] }))

  test('the draft starts from the effective kind and every chosen topic', () => {
    expect(initial).toEqual({ kind: 'bulk', topics: ['purchases', 'health'] })
  })

  test('nothing changed and nothing stored means nothing to send', () => {
    expect(overrideRequest('address', 'ada@example.com', initial, initial, undefined)).toBe(
      undefined,
    )
  })

  test('only the changed axis is sent', () => {
    expect(
      overrideRequest(
        'address',
        'ada@example.com',
        initial,
        { ...initial, kind: 'notification' },
        undefined,
      ),
    ).toEqual({ scope: 'address', key: 'ada@example.com', kind: 'notification' })
  })

  test('topics go in taxonomy order and keep a hidden health choice', () => {
    expect(
      overrideRequest(
        'domain',
        'example.com',
        initial,
        { ...initial, topics: ['travel', 'health', 'purchases'] },
        undefined,
      ),
    ).toEqual({ scope: 'domain', key: 'example.com', topics: ['purchases', 'travel', 'health'] })
  })

  test('the other axis of the replaced override is kept', () => {
    expect(
      overrideRequest(
        'address',
        'ada@example.com',
        initial,
        { ...initial, kind: 'personal' },
        override({ topics: ['events'] }),
      ),
    ).toEqual({ scope: 'address', key: 'ada@example.com', kind: 'personal', topics: ['events'] })
  })
})

describe('optimistic apply and held-back text', () => {
  test('sensitive topics are held back on mail that is not aligned', () => {
    const next = applyOverride(insights({ trust: { state: 'unverified' } }), {
      kind: 'personal',
      topics: ['finance', 'travel'],
    })
    expect(next.kind).toEqual({ id: 'personal', source: 'override', overridden: true })
    expect(next.topics).toEqual([
      { id: 'travel', source: 'override', overridden: true },
      { id: 'finance', source: 'override', overridden: true, held_back: 'not_verified' },
    ])
    expect(heldBackText(next.topics)).toBe('Not applied here: sender not verified (Finance)')
    expect(cardChips(next).topics.map((chip) => chip.id)).toEqual(['travel'])
  })

  test('aligned mail gets sensitive topics', () => {
    const next = applyOverride(insights({ trust: { state: 'aligned', domain: 'example.com' } }), {
      topics: ['government'],
    })
    expect(next.kind?.id).toBe('bulk')
    expect(next.topics[0]?.held_back).toBeUndefined()
    expect(heldBackText(next.topics)).toBeUndefined()
  })

  test('a held-back health topic is not named', () => {
    expect(heldBackText([topic('health', { held_back: 'not_verified' })])).toBeUndefined()
  })
})

const name = (id: string) => (id === 'simplelogin' ? 'SimpleLogin' : id)

const target = (id: string, extra: Partial<MessageInsights> | null = {}) => ({
  id,
  from: [{ email: 'ada@example.com' }],
  insights: extra === null ? null : insights(extra),
})

const all = () => true

describe('trust notes', () => {
  test('each state gets its note', () => {
    expect(trustNote({ state: 'aligned', domain: 'example.com' }, name)).toBeUndefined()
    expect(trustNote({ state: 'unknown' }, name)).toEqual({
      tone: 'muted',
      text: 'Sender not checked',
    })
    expect(trustNote({ state: 'unverified' }, name)).toEqual({
      tone: 'warning',
      text: 'Sender not verified',
    })
    expect(trustNote({ state: 'relayed', provider: 'simplelogin' }, name)?.text).toBe(
      'via SimpleLogin, original sender not verified',
    )
  })
})

describe('category target', () => {
  test('the focused message wins, else the newest expanded one', () => {
    const messages = [target('a'), target('b'), target('c', { status: 'pending' })]
    expect(categoryTarget(messages, all, 'a')?.id).toBe('a')
    expect(categoryTarget(messages, all, undefined)?.id).toBe('b')
    expect(categoryTarget(messages, (id) => id === 'a', undefined)?.id).toBe('a')
    expect(categoryTarget(messages, all, 'c')).toBeUndefined()
  })

  test('mail without insights cannot be categorised', () => {
    expect(canCategorize(target('a', null))).toBe(false)
  })
})

describe('settings and trusted servers', () => {
  test('an override summary names scope, kind and topics', () => {
    expect(overrideSummary(override({ kind: 'bulk', topics: ['travel', 'health'] }))).toEqual({
      scope: 'Address',
      kind: 'Newsletter',
      topics: 'Travel',
    })
    expect(overrideSummary(override({ scope: 'domain', topics: [] }))).toEqual({
      scope: 'Domain',
      kind: 'As found',
      topics: 'No topics',
    })
  })

  test('trusted server ids are checked like the server does', () => {
    expect(trustedIdError(' mx.example.org ', [])).toBeUndefined()
    expect(trustedIdError('', [])).toBe('Enter a server name.')
    expect(trustedIdError('mx example', [])).toBe(
      'Use printable characters without spaces, at most 255.',
    )
    expect(trustedIdError('MX.example.org', ['mx.example.org'])).toBe(
      'This server is already in the list.',
    )
    const full = Array.from({ length: 16 }, (_, i) => `mx${i}.example.org`)
    expect(trustedIdError('mx.example.net', full)).toBe('At most 16 servers.')
  })
})
