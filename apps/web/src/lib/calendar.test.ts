import { describe, expect, test } from 'bun:test'

import {
  addDays,
  addMonths,
  byDay,
  calendarColor,
  dayKeyAt,
  dayMinutes,
  isDayKey,
  layoutColumns,
  occurrenceDays,
  shiftAnchor,
  shownCalendars,
  startOfWeek,
  viewSpan,
  wallClock,
  zonedMidnight,
} from './calendar.ts'

const utc = (iso: string) => Date.parse(iso)

const timed = (start: string, end: string) => ({
  starts_at: utc(start),
  ends_at: utc(end),
  all_day: false,
  floating: false,
  start_local: '',
  end_local: '',
})

describe('day keys', () => {
  test('add days and months across month ends and leap years', () => {
    expect(addDays('2024-02-28', 1)).toBe('2024-02-29')
    expect(addDays('2025-03-01', -1)).toBe('2025-02-28')
    expect(addMonths('2025-01-31', 1)).toBe('2025-02-28')
    expect(addMonths('2024-03-31', -1)).toBe('2024-02-29')
    expect(addMonths('2025-12-15', 1)).toBe('2026-01-15')
    expect(isDayKey('2025-02-29')).toBe(false)
    expect(isDayKey('2025-03-30')).toBe(true)
  })

  test('weeks start on Monday by default', () => {
    expect(startOfWeek('2025-03-30')).toBe('2025-03-24')
    expect(startOfWeek('2025-03-24')).toBe('2025-03-24')
    expect(startOfWeek('2025-03-30', 0)).toBe('2025-03-30')
  })
})

describe('zones', () => {
  test('midnight moves with daylight saving time', () => {
    expect(zonedMidnight('2025-03-30', 'Europe/Berlin')).toBe(utc('2025-03-29T23:00:00Z'))
    expect(zonedMidnight('2025-03-31', 'Europe/Berlin')).toBe(utc('2025-03-30T22:00:00Z'))
    expect(zonedMidnight('2025-11-02', 'America/New_York')).toBe(utc('2025-11-02T04:00:00Z'))
    expect(zonedMidnight('2025-11-03', 'America/New_York')).toBe(utc('2025-11-03T05:00:00Z'))
    expect(zonedMidnight('2025-03-30', 'UTC')).toBe(utc('2025-03-30T00:00:00Z'))
  })

  test('reads the wall clock of an instant', () => {
    expect(wallClock(utc('2025-03-30T01:30:00Z'), 'Europe/Berlin')).toEqual({
      day: '2025-03-30',
      minutes: 3 * 60 + 30,
    })
    expect(dayKeyAt(utc('2025-03-30T23:30:00Z'), 'America/New_York')).toBe('2025-03-30')
    expect(dayKeyAt(utc('2025-03-30T23:30:00Z'), 'Asia/Tokyo')).toBe('2025-03-31')
  })
})

describe('viewSpan', () => {
  test('a week across the spring change is 167 hours long', () => {
    const span = viewSpan('week', '2025-03-27', 'Europe/Berlin')
    expect(span.days).toEqual([
      '2025-03-24',
      '2025-03-25',
      '2025-03-26',
      '2025-03-27',
      '2025-03-28',
      '2025-03-29',
      '2025-03-30',
    ])
    expect(span.from).toBe(utc('2025-03-23T23:00:00Z'))
    expect(span.to).toBe(utc('2025-03-30T22:00:00Z'))
    expect((span.to - span.from) / 3_600_000).toBe(167)
  })

  test('the month view shows whole weeks and the agenda the month', () => {
    const month = viewSpan('month', '2025-03-15', 'UTC')
    expect(month.days[0]).toBe('2025-02-24')
    expect(month.days.at(-1)).toBe('2025-04-06')
    expect(month.days.length % 7).toBe(0)
    const agenda = viewSpan('agenda', '2025-02-10', 'UTC')
    expect(agenda.days.length).toBe(28)
    expect(agenda.to).toBe(utc('2025-03-01T00:00:00Z'))
  })

  test('steps by a week or a month', () => {
    expect(shiftAnchor('week', '2025-03-27', 1)).toBe('2025-04-03')
    expect(shiftAnchor('month', '2025-01-31', 1)).toBe('2025-02-28')
    expect(shiftAnchor('agenda', '2025-03-10', -1)).toBe('2025-02-10')
  })
})

describe('placing instances', () => {
  test('an all-day instance keeps its dates in every zone', () => {
    const allDay = {
      starts_at: utc('2025-03-30T00:00:00Z'),
      ends_at: utc('2025-04-01T00:00:00Z'),
      all_day: true,
      floating: false,
      start_local: '2025-03-30',
      end_local: '2025-04-01',
    }
    expect(occurrenceDays(allDay, 'America/Los_Angeles')).toEqual(['2025-03-30', '2025-03-31'])
    expect(occurrenceDays(allDay, 'Asia/Tokyo')).toEqual(['2025-03-30', '2025-03-31'])
  })

  test('a timed instance follows the viewer zone and stops at midnight', () => {
    const late = timed('2025-03-30T21:30:00Z', '2025-03-30T22:30:00Z')
    expect(occurrenceDays(late, 'UTC')).toEqual(['2025-03-30'])
    expect(occurrenceDays(late, 'Europe/Berlin')).toEqual(['2025-03-30', '2025-03-31'])
    expect(dayMinutes(late, '2025-03-30', 'Europe/Berlin')).toEqual({
      start: 23 * 60 + 30,
      end: 1440,
    })
    expect(dayMinutes(late, '2025-03-31', 'Europe/Berlin')).toEqual({ start: 0, end: 30 })
    const toMidnight = timed('2025-03-30T20:00:00Z', '2025-03-30T22:00:00Z')
    expect(occurrenceDays(toMidnight, 'Europe/Berlin')).toEqual(['2025-03-30'])
  })

  test('floating times read the same everywhere', () => {
    const floating = {
      starts_at: utc('2025-06-01T08:00:00Z'),
      ends_at: utc('2025-06-01T08:30:00Z'),
      all_day: false,
      floating: true,
      start_local: '2025-06-01T08:00:00',
      end_local: '2025-06-01T08:30:00',
    }
    expect(dayMinutes(floating, '2025-06-01', 'Asia/Tokyo')).toEqual({ start: 480, end: 510 })
  })

  test('groups by day, all-day first', () => {
    const allDay = {
      ...timed('2025-03-30T00:00:00Z', '2025-03-31T00:00:00Z'),
      all_day: true,
      start_local: '2025-03-30',
      end_local: '2025-03-31',
    }
    const morning = timed('2025-03-30T07:00:00Z', '2025-03-30T08:00:00Z')
    const days = byDay([morning, allDay], ['2025-03-30', '2025-03-31'], 'UTC')
    expect(days.get('2025-03-30')).toEqual([allDay, morning])
    expect(days.get('2025-03-31')).toEqual([])
  })
})

describe('layoutColumns', () => {
  test('overlapping blocks share columns, separate clusters start over', () => {
    const slots = layoutColumns([
      { id: 'a', start: 540, end: 600 },
      { id: 'b', start: 570, end: 630 },
      { id: 'c', start: 600, end: 660 },
      { id: 'd', start: 720, end: 780 },
    ])
    expect(slots.get('a')).toEqual({ column: 0, columns: 2 })
    expect(slots.get('b')).toEqual({ column: 1, columns: 2 })
    expect(slots.get('c')).toEqual({ column: 0, columns: 2 })
    expect(slots.get('d')).toEqual({ column: 0, columns: 1 })
  })

  test('three at once take three columns and short ones get a minimum height', () => {
    const slots = layoutColumns([
      { id: 'x', start: 600, end: 600 },
      { id: 'y', start: 605, end: 610 },
      { id: 'z', start: 600, end: 700 },
    ])
    expect(slots.get('z')).toEqual({ column: 0, columns: 3 })
    expect(slots.get('x')).toEqual({ column: 1, columns: 3 })
    expect(slots.get('y')).toEqual({ column: 2, columns: 3 })
  })
})

describe('calendars', () => {
  test('accepts only hex colors', () => {
    expect(calendarColor('#2A9D8F')).toBe('#2A9D8F')
    expect(calendarColor('#2a9d8fff')).toBe('#2a9d8f')
    expect(calendarColor('red; background: url(x)')).toBeUndefined()
    expect(calendarColor(null)).toBeUndefined()
  })

  test('asks for every calendar unless some are hidden', () => {
    const all = [{ id: 'a' }, { id: 'b' }]
    expect(shownCalendars(all, [])).toBeUndefined()
    expect(shownCalendars(all, ['a'])).toEqual(['b'])
    expect(shownCalendars(all, ['a', 'b'])).toEqual([])
  })
})
