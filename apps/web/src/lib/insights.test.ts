import { describe, expect, test } from 'bun:test'

import type {
  ActionInsight,
  CodeInsight,
  Insight,
  Invitation,
  InvoiceInsight,
  MessageInsights,
  OrderInsight,
  ParcelInsight,
  PassInsight,
  Reservation,
  ReservationInsight,
  UnsubscribeInsight,
} from '@holtmail/api-client'

import {
  actionBadges,
  actionLabel,
  cardView,
  carrierName,
  codeAge,
  copyTarget,
  codeTime,
  copyValue,
  formatInsightDate,
  formatInsightTime,
  formatMoney,
  formatPassDate,
  groupCode,
  insightInstant,
  isDuplicateOfInvitation,
  passParts,
  purchaseParts,
  reservationParts,
  isStale,
  linkTarget,
  orderInsights,
  primaryAction,
  reasonText,
  STALE_CODE_MS,
  type TargetMessage,
  topCopy,
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

const action = (extra: Partial<ActionInsight> = {}): ActionInsight => ({
  type: 'action',
  action: 'reset_password',
  url: 'https://account.example.com/reset?t=1',
  destination: 'https://account.example.com/reset?t=1',
  display_host: 'account.example.com',
  via: null,
  tracked: false,
  text_domain_mismatch: false,
  destination_differs_from_sender: false,
  confusable: false,
  label: null,
  confidence: 'likely',
  score: 700,
  forwarded: false,
  ...extra,
})

const parcel = (number: string, extra: Partial<ParcelInsight> = {}): ParcelInsight => ({
  type: 'parcel',
  carrier: 'ups',
  number,
  check_digit: true,
  track_url: `https://track.example.net/ups/${number}`,
  track_host: 'track.example.net',
  confidence: 'likely',
  score: 900,
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
  kind: null,
  topics: [],
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
    const reasons = [
      'no_post_header',
      'not_https',
      'multiple_fields',
      'not_covered',
      'not_aligned',
      'no_trusted_authserv',
      'disabled',
      'recently_requested',
      'members',
    ] as const
    for (const reason of reasons) expect(reasonText(reason)?.length).toBeGreaterThan(0)
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
    expect(copyTarget(thread, all, undefined, now)?.id).toBe('b')
  })

  test('picks the newest code when every one is stale', () => {
    expect(copyTarget(thread, all, undefined, now + 60 * minute)?.id).toBe('d')
  })

  test('only looks at expanded messages', () => {
    expect(copyTarget(thread, (id) => id !== 'b', undefined, now)?.id).toBe('d')
  })

  test('the focused message wins, even with nothing to copy', () => {
    expect(copyTarget(thread, all, 'a', now)?.id).toBe('a')
    expect(copyTarget(thread, all, 'c', now)).toBeUndefined()
  })

  test('picks the newest message with an unsubscribe link', () => {
    expect(unsubscribeTarget(thread, all, undefined)?.id).toBe('c')
    expect(unsubscribeTarget(thread, all, 'a')).toBeUndefined()
  })
})

const describeItem = (item: Insight) =>
  item.type === 'code'
    ? item.value
    : item.type === 'action'
      ? item.action
      : item.type === 'parcel'
        ? item.number
        : item.type

describe('actions and parcels', () => {
  test('orders codes, then actions, then parcels', () => {
    const ordered = orderInsights([
      parcel('1Z999AA10123456784'),
      unsubscribeItem(),
      action({ action: 'view_order' }),
      code('482913'),
    ])
    expect(ordered.map(describeItem)).toEqual(['482913', 'view_order', '1Z999AA10123456784'])
  })

  test('the primary action has the highest confidence, then score', () => {
    const items = [
      action({ action: 'view', score: 950 }),
      action({ action: 'sign_in', confidence: 'certain', score: 600 }),
      action({ action: 'track', confidence: 'certain', score: 800 }),
    ]
    expect(primaryAction(items)?.action).toBe('track')
    expect(primaryAction([code('482913')])).toBeUndefined()
    const view = cardView(insights(items))
    expect(view?.action?.action).toBe('track')
  })

  test("the sender's own action comes before a forwarded one, for the row and Shift+L", () => {
    const forwarded = action({
      action: 'confirm',
      url: 'https://booking.example.org/confirm/1',
      confidence: 'certain',
      score: 1000,
      forwarded: true,
    })
    const own = action({ action: 'confirm', url: 'https://example.com/confirm/2' })
    expect(primaryAction([forwarded, own])?.url).toBe('https://example.com/confirm/2')
    expect(cardView(insights([forwarded, own]))?.link?.href).toBe('https://example.com/confirm/2')
    expect(primaryAction([forwarded])?.url).toBe('https://booking.example.org/confirm/1')
  })

  test('labels actions and carriers, falling back to the raw id', () => {
    expect(actionLabel('reset_password')).toBe('Reset password')
    expect(actionLabel('wallet_save')).toBe('Save to wallet')
    expect(carrierName('dhl_express')).toBe('DHL Express')
    expect(carrierName('pigeon_post')).toBe('pigeon_post')
  })

  test('a card shows actions and parcels without codes', () => {
    const view = cardView(insights([parcel('1Z999AA10123456784'), parcel('EE123456785US')]))
    expect(view?.parcels.map((item) => item.number)).toEqual([
      '1Z999AA10123456784',
      'EE123456785US',
    ])
    expect(view?.action).toBeUndefined()
  })

  test('a member sees only the actions the server left', () => {
    const view = cardView(
      insights([action({ action: 'view_order' }), parcel('1Z999AA10123456784')], {
        unsubscribe: { one_click: { available: false, reason: 'members' } },
      }),
    )
    expect(view?.codes).toEqual([])
    expect(view?.action?.action).toBe('view_order')
    expect(view?.unsubscribe).toBeUndefined()
    expect(view?.copy).toEqual({ kind: 'tracking', value: '1Z999AA10123456784' })
  })
})

const badgeKeys = (item: ActionInsight, state: MessageInsights['trust']['state'] = 'unknown') =>
  actionBadges(item, { state }).map((badge) => badge.key)

describe('badges', () => {
  const unknown = { state: 'unknown' } as const
  test('shows none for a plain link', () => {
    expect(actionBadges(action(), unknown)).toEqual([])
  })

  test.each([
    ['tracked', { tracked: true }, false, 'Tracked'],
    ['mismatch', { text_domain_mismatch: true }, true, 'Link text names another site'],
    [
      'differs',
      { destination_differs_from_sender: true },
      true,
      'Goes to another site than the sender',
    ],
    ['confusable', { confusable: true }, true, 'Look-alike address'],
  ] as const)('%s', (key, flags, warning, text) => {
    expect(actionBadges(action(flags), unknown)).toEqual([{ key, text, warning }])
  })

  test('sender not verified only on unverified and relayed mail', () => {
    expect(badgeKeys(action(), 'unverified')).toEqual(['unverified'])
    expect(badgeKeys(action(), 'relayed')).toEqual(['unverified'])
    expect(badgeKeys(action(), 'aligned')).toEqual([])
    expect(badgeKeys(action(), 'unknown')).toEqual([])
    expect(cardView(insights([action()], { trust: { state: 'relayed' } }))?.unverified).toBe(true)
  })

  test('keeps a fixed order when several flags are set', () => {
    const flagged = action({
      tracked: true,
      text_domain_mismatch: true,
      destination_differs_from_sender: true,
      confusable: true,
    })
    expect(badgeKeys(flagged, 'unverified')).toEqual([
      'tracked',
      'mismatch',
      'differs',
      'confusable',
      'unverified',
    ])
  })
})

describe('copy and link fallbacks', () => {
  const tracking = parcel('1Z999AA10123456784')

  test('c copies the code first, else the tracking number', () => {
    const both = message('a', 1, [tracking, code('482 913')])
    expect(topCopy(both)).toEqual({ kind: 'code', value: '482913' })
    expect(topCopy(message('b', 1, [tracking]))).toEqual({
      kind: 'tracking',
      value: '1Z999AA10123456784',
    })
    expect(topCopy(message('c', 1, [action()]))).toBeUndefined()
  })

  test('c picks a fresh code over a newer tracking number, else the newest', () => {
    const thread = [message('a', 5, [code('111111')]), message('b', 1, [tracking])]
    expect(copyTarget(thread, all, undefined, now)?.id).toBe('a')
    expect(copyTarget(thread, all, undefined, now + 60 * minute)?.id).toBe('b')
  })

  test('Shift+L opens the action, else the tracking link', () => {
    expect(cardView(insights([tracking, action()]))?.link).toEqual({
      href: 'https://account.example.com/reset?t=1',
      kind: 'action',
    })
    expect(
      cardView(insights([parcel('EE123456785US', { track_url: null }), tracking]))?.link,
    ).toEqual({
      href: 'https://track.example.net/ups/1Z999AA10123456784',
      kind: 'tracking',
    })
    expect(cardView(insights([parcel('EE123456785US', { track_url: null })]))?.link).toBeUndefined()
  })

  test('Shift+L picks the focused message, else the newest expanded one with a link', () => {
    const thread = [
      message('a', 30, [action()]),
      message('b', 20, [tracking]),
      message('c', 10, [code('482913')]),
    ]
    expect(linkTarget(thread, all, undefined)?.id).toBe('b')
    expect(linkTarget(thread, (id) => id !== 'b', undefined)?.id).toBe('a')
    expect(linkTarget(thread, all, 'a')?.id).toBe('a')
    expect(linkTarget(thread, all, 'c')).toBeUndefined()
  })
})

const flight = (extra: Partial<Extract<Reservation, { kind: 'flight' }>> = {}): Reservation => ({
  kind: 'flight',
  reservation_number: 'ABC123',
  carrier: 'LH',
  flight_number: '1234',
  departure_airport: 'FRA',
  arrival_airport: 'LIS',
  departure: { local: '2026-03-12T07:15:00', offset: '+01:00', zone: 'Europe/Berlin' },
  arrival: null,
  passengers: ['Ada Example'],
  seats: ['14C'],
  url: 'https://air.example.com/trip/ABC123',
  url_host: 'air.example.com',
  ...extra,
})

const concert = (start: Extract<Reservation, { kind: 'event' }>['start']): Reservation => ({
  kind: 'event',
  name: 'Example Quartet',
  start,
  venue: 'Hall One',
  tickets: [],
})

const reservation = (
  value: Reservation,
  extra: Partial<ReservationInsight> = {},
): ReservationInsight => ({
  type: 'reservation',
  reservation: value,
  confidence: 'certain',
  score: 1000,
  forwarded: false,
  ...extra,
})

const pass = (extra: Partial<PassInsight> = {}): PassInsight => ({
  type: 'pass',
  style: 'boarding_pass',
  organization: 'Example Air',
  description: 'Boarding pass',
  relevant: null,
  boarding: {
    pnr: 'XYZ789',
    passenger: 'Ada Example',
    legs: [
      {
        from: 'FRA',
        to: 'LIS',
        carrier: 'LH',
        flight: '1234',
        date: { month: 3, day: 12 },
        seat: '14C',
      },
    ],
  },
  attachment: 0,
  signature_checked: false,
  confidence: 'certain',
  score: 1000,
  forwarded: false,
  ...extra,
})

const order = (extra: Partial<OrderInsight> = {}): OrderInsight => ({
  type: 'order',
  number: 'ORD-100',
  merchant: 'Example Shop',
  total: { amount: '1234.50', currency: 'USD', exponent: 2 },
  status: 'Shipped',
  url: null,
  url_host: null,
  confidence: 'certain',
  score: 1000,
  forwarded: false,
  ...extra,
})

const invoice = (extra: Partial<InvoiceInsight> = {}): InvoiceInsight => ({
  type: 'invoice',
  number: 'INV-7',
  payee: 'Example Utilities',
  total: { amount: '42.000', currency: 'KWD', exponent: 3 },
  due: '2026-04-01',
  url: null,
  url_host: null,
  confidence: 'certain',
  score: 1000,
  forwarded: false,
  ...extra,
})

const invitation = (extra: Partial<Invitation> = {}): Invitation => ({
  item_id: 'item-1',
  version: 1,
  version_count: 1,
  updates_after: 0,
  all_day: false,
  recurring: false,
  cancelled: false,
  start: {
    value: '2026-05-20T19:30:00',
    time_zone: 'Europe/Amsterdam',
    utc: Date.UTC(2026, 4, 20, 17, 30),
  },
  ...extra,
})

const withZone = <T>(zone: string, run: () => T): T => {
  const saved = process.env.TZ
  process.env.TZ = zone
  try {
    return run()
  } finally {
    if (saved === undefined) delete process.env.TZ
    else process.env.TZ = saved
  }
}

describe('reservations, passes, orders and invoices', () => {
  test('orders every kind: code, action, reservation, pass, parcel, order, invoice', () => {
    const ordered = orderInsights([
      invoice(),
      order(),
      parcel('1Z999AA10123456784'),
      pass(),
      reservation(flight()),
      unsubscribeItem(),
      action(),
      code('482913'),
    ])
    expect(ordered.map((item) => item.type)).toEqual([
      'code',
      'action',
      'reservation',
      'pass',
      'parcel',
      'order',
      'invoice',
    ])
    const view = cardView(insights([invoice(), order(), pass(), reservation(flight())]))
    expect([
      view?.reservations.length,
      view?.passes.length,
      view?.orders.length,
      view?.invoices.length,
    ]).toEqual([1, 1, 1, 1])
  })

  test('a flight row reads like the boarding card', () => {
    expect(reservationParts(flight(), 2026)).toEqual([
      'LH 1234',
      'FRA → LIS',
      '12 Mar 07:15 CET',
      'Seat 14C',
    ])
    expect(reservationParts(flight({ seats: ['14C', '14D'], carrier: null }), 2026)).toEqual([
      '1234',
      'FRA → LIS',
      '12 Mar 07:15 CET',
      'Seats 14C, 14D',
    ])
  })

  test('other kinds show their own fields', () => {
    const local = { local: '2026-06-01T15:00:00', offset: null, zone: null }
    expect(
      reservationParts(
        {
          kind: 'lodging',
          reservation_number: 'H-1',
          name: 'Example Inn',
          checkin: local,
          checkout: { ...local, local: '2026-06-03T11:00:00' },
          guests: [],
        },
        2026,
      ),
    ).toEqual(['Example Inn', '1 Jun 15:00 → 3 Jun 11:00'])
    expect(
      reservationParts(
        {
          kind: 'lodging',
          reservation_number: 'H-1',
          name: 'Example Inn',
          checkin: local,
          guests: [],
        },
        2026,
      ),
    ).toEqual(['Example Inn', 'Check-in 1 Jun 15:00'])
    expect(
      reservationParts(
        {
          kind: 'train',
          number: 'ICE 71',
          departure_station: 'Example Hbf',
          arrival_station: 'Sample Central',
          departure: local,
          passengers: [],
          seats: ['41'],
        },
        2026,
      ),
    ).toEqual(['ICE 71', 'Example Hbf → Sample Central', '1 Jun 15:00', 'Seat 41'])
    expect(
      reservationParts({ kind: 'food', name: 'Example Bistro', start: local, party_size: 4 }, 2026),
    ).toEqual(['Example Bistro', '1 Jun 15:00', '4 people'])
    expect(
      reservationParts(
        {
          kind: 'rental_car',
          reservation_number: 'R-9',
          company: 'Example Cars',
          pickup: local,
          pickup_location: 'LIS',
        },
        2026,
      ),
    ).toEqual(['Example Cars', '1 Jun 15:00', 'LIS'])
    expect(reservationParts(concert(local), 2026)).toEqual([
      'Example Quartet',
      '1 Jun 15:00',
      'Hall One',
    ])
  })

  test('passes show legs and seats, orders and invoices their totals', () => {
    expect(passParts(pass(), 2026)).toEqual([
      'Example Air',
      'Boarding pass',
      'LH 1234 FRA → LIS 12 Mar Seat 14C',
    ])
    expect(purchaseParts(order())).toEqual(['Example Shop', '$1,234.50', 'Shipped'])
    expect(purchaseParts(invoice(), 2026)).toEqual([
      'Example Utilities',
      'KWD\u00a042.000',
      'Due 1 Apr',
    ])
  })
})

const at = (local: string, offset: string | null, zone: string | null = null) => ({
  local,
  offset,
  zone,
})

describe('formatInsightTime', () => {
  test('shows the zone abbreviation when the zone agrees with the offset', () => {
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+01:00', 'Europe/Berlin'), 2026)).toBe(
      '12 Mar 07:15 CET',
    )
    expect(formatInsightTime(at('2026-07-12T07:15:00', '-04:00', 'America/New_York'), 2026)).toBe(
      '12 Jul 07:15 EDT',
    )
  })

  test('falls back to the offset when the zone has no abbreviation or disagrees', () => {
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+05:30', 'Asia/Kolkata'), 2026)).toBe(
      '12 Mar 07:15 UTC+05:30',
    )
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+02:00', 'Europe/Berlin'), 2026)).toBe(
      '12 Mar 07:15 UTC+02:00',
    )
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+01:00', 'Not/AZone'), 2026)).toBe(
      '12 Mar 07:15 UTC+01:00',
    )
  })

  test('shows an offset alone, UTC for zero, nothing for a Local time', () => {
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+01:00'), 2026)).toBe(
      '12 Mar 07:15 UTC+01:00',
    )
    expect(formatInsightTime(at('2026-03-12T07:15:00', '-09:30'), 2026)).toBe(
      '12 Mar 07:15 UTC-09:30',
    )
    expect(formatInsightTime(at('2026-03-12T07:15:00', '+00:00'), 2026)).toBe('12 Mar 07:15 UTC')
    expect(formatInsightTime(at('2026-03-12T07:15:00', null), 2026)).toBe('12 Mar 07:15')
    expect(formatInsightTime(at('2026-03-12T07:15', null), 2026)).toBe('12 Mar 07:15')
  })

  test('adds the year when it is not the message year or that is unknown', () => {
    expect(formatInsightTime(at('2027-01-02T23:59:00', null), 2026)).toBe('2 Jan 2027 23:59')
    expect(formatInsightTime(at('2026-01-02T23:59:00', null))).toBe('2 Jan 2026 23:59')
    expect(formatInsightDate('2026-04-01', 2026)).toBe('1 Apr')
    expect(formatPassDate({ month: 3, day: 12 }, 2025)).toBe('12 Mar')
    expect(formatPassDate('2026-03-12', 2025)).toBe('12 Mar 2026')
  })

  test('never shifts with the viewer zone', () => {
    const times = [
      at('2026-03-12T07:15:00', '+01:00', 'Europe/Berlin'),
      at('2026-03-12T23:30:00', '-10:00'),
      at('2026-03-12T00:05:00', null),
    ]
    const shown = times.map((time) => formatInsightTime(time, 2026))
    expect(withZone('Pacific/Kiritimati', () => new Date(Date.UTC(2026, 2, 12)).getHours())).toBe(
      14,
    )
    for (const zone of ['Pacific/Kiritimati', 'Pacific/Pago_Pago', 'Asia/Kathmandu']) {
      expect(withZone(zone, () => times.map((time) => formatInsightTime(time, 2026)))).toEqual(
        shown,
      )
    }
    expect(shown).toEqual(['12 Mar 07:15 CET', '12 Mar 23:30 UTC-10:00', '12 Mar 00:05'])
    expect(insightInstant(times[1]!)).toBe(Date.UTC(2026, 2, 13, 9, 30))
    expect(insightInstant(times[2]!)).toBeUndefined()
  })
})

describe('formatMoney', () => {
  test.each([
    [{ amount: '1200', currency: 'JPY', exponent: 0 }, '¥1,200'],
    [{ amount: '1234.50', currency: 'USD', exponent: 2 }, '$1,234.50'],
    [{ amount: '3.000', currency: 'KWD', exponent: 3 }, 'KWD\u00a03.000'],
    [{ amount: '-1234.50', currency: 'USD', exponent: 2 }, '-$1,234.50'],
    [{ amount: '9223372036854775807', currency: 'JPY', exponent: 0 }, '¥9,223,372,036,854,775,807'],
    [
      { amount: '92233720368547758.07', currency: 'USD', exponent: 2 },
      '$92,233,720,368,547,758.07',
    ],
    [{ amount: '12.50', currency: 'XY1', exponent: 2 }, '12.50 XY1'],
  ])('%o shows as %s', (money, shown) => {
    expect(formatMoney(money)).toBe(shown)
  })
})

describe('booking reference copy', () => {
  test('c copies the code, else the booking reference, else the pass PNR, else tracking', () => {
    const tracking = parcel('1Z999AA10123456784')
    const flightItem = reservation(flight())
    expect(topCopy(message('a', 1, [tracking, flightItem, code('482913')]))).toEqual({
      kind: 'code',
      value: '482913',
    })
    expect(topCopy(message('b', 1, [tracking, pass(), flightItem]))).toEqual({
      kind: 'reference',
      value: 'ABC123',
    })
    expect(
      topCopy(
        message('c', 1, [tracking, pass(), reservation(flight({ reservation_number: null }))]),
      ),
    ).toEqual({ kind: 'reference', value: 'XYZ789' })
    expect(topCopy(message('d', 1, [tracking, pass({ boarding: null })]))).toEqual({
      kind: 'tracking',
      value: '1Z999AA10123456784',
    })
  })

  test('references copy as written', () => {
    const spaced = reservation(flight({ reservation_number: 'AB 12 CD' }))
    expect(topCopy(message('a', 1, [spaced]))).toEqual({ kind: 'reference', value: 'AB 12 CD' })
  })

  test('a hidden event reservation gives no reference', () => {
    const event = reservation({
      ...concert({ local: '2026-05-20T19:30:00', offset: '+02:00', zone: 'Europe/Amsterdam' }),
      reservation_number: 'EV-1',
    })
    const target: TargetMessage = {
      ...message('a', 1, [event, parcel('EE123456785US')]),
      invitations: [invitation()],
    }
    expect(topCopy(target)).toEqual({ kind: 'tracking', value: 'EE123456785US' })
    expect(topCopy({ ...target, invitations: [] })).toEqual({ kind: 'reference', value: 'EV-1' })
  })
})

const zoned = (local: string) => concert({ local, offset: '+02:00', zone: 'Europe/Amsterdam' })
const local = (value: string) => concert({ local: value, offset: null, zone: null })

describe('isDuplicateOfInvitation', () => {
  test('a zoned start matches as an instant within one minute, inclusive', () => {
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:30:00'), [invitation()])).toBe(true)
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:31:00'), [invitation()])).toBe(true)
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:29:00'), [invitation()])).toBe(true)
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:31:01'), [invitation()])).toBe(false)
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:28:59'), [invitation()])).toBe(false)
    const utc = concert({ local: '2026-05-20T17:30:00', offset: '+00:00', zone: null })
    expect(isDuplicateOfInvitation(utc, [invitation()])).toBe(true)
  })

  test('a Local start matches as wall time on the invitation zone', () => {
    expect(isDuplicateOfInvitation(local('2026-05-20T19:30:00'), [invitation()])).toBe(true)
    expect(isDuplicateOfInvitation(local('2026-05-20T17:30:00'), [invitation()])).toBe(false)
    const written = invitation({
      start: {
        value: '2026-05-20T17:30:00Z',
        time_zone: 'Europe/Amsterdam',
        utc: Date.UTC(2026, 4, 20, 17, 30),
      },
    })
    expect(isDuplicateOfInvitation(local('2026-05-20T19:30:00'), [written])).toBe(true)
    const floating = invitation({
      start: { value: '2026-05-20T19:30:00', time_zone: null, utc: null },
    })
    expect(isDuplicateOfInvitation(local('2026-05-20T19:30:30'), [floating])).toBe(true)
    expect(isDuplicateOfInvitation(local('2026-05-20T19:31:30'), [floating])).toBe(false)
  })

  test('all-day invitations, missing starts and other kinds never match', () => {
    const allDay = invitation({
      all_day: true,
      start: { value: '2026-05-20', time_zone: null, utc: null },
    })
    expect(isDuplicateOfInvitation(local('2026-05-20T00:00:00'), [allDay])).toBe(false)
    expect(
      isDuplicateOfInvitation(zoned('2026-05-20T19:30:00'), [invitation({ start: null })]),
    ).toBe(false)
    expect(isDuplicateOfInvitation(zoned('2026-05-20T19:30:00'), [])).toBe(false)
    const sameTime = flight({
      departure: { local: '2026-05-20T19:30:00', offset: '+02:00', zone: null },
    })
    expect(isDuplicateOfInvitation(sameTime, [invitation()])).toBe(false)
  })

  test('the card hides the duplicate and keeps the rest', () => {
    const items = [reservation(zoned('2026-05-20T19:30:00')), reservation(flight())]
    const view = cardView(insights(items), [invitation()])
    expect(view?.reservations.map((item) => item.reservation.kind)).toEqual(['flight'])
    expect(cardView(insights([items[0]!]), [invitation()])).toBeUndefined()
  })
})

describe('forwarded items', () => {
  test('keep their forwarded flag for the badge', () => {
    const view = cardView(
      insights([
        reservation(flight(), { forwarded: true }),
        pass({ forwarded: true }),
        order({ forwarded: true }),
        invoice(),
      ]),
    )
    expect(view?.reservations[0]?.forwarded).toBe(true)
    expect(view?.passes[0]?.forwarded).toBe(true)
    expect(view?.orders[0]?.forwarded).toBe(true)
    expect(view?.invoices[0]?.forwarded).toBe(false)
  })
})
