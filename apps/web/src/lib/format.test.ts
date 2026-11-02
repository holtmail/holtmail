import { describe, expect, test } from 'bun:test'

import { daysBetween, formatListDate, formatSize } from './format.ts'

describe('formatSize', () => {
  test('bytes stay whole', () => {
    expect(formatSize(0, 'en')).toBe('0 B')
    expect(formatSize(512, 'en')).toBe('512 B')
    expect(formatSize(1023, 'en')).toBe('1,023 B')
  })

  test('small units keep one decimal, large ones none', () => {
    expect(formatSize(1024, 'en')).toBe('1 KB')
    expect(formatSize(1536, 'en')).toBe('1.5 KB')
    expect(formatSize(20 * 1024 + 300, 'en')).toBe('20 KB')
    expect(formatSize(5.25 * 1024 * 1024, 'en')).toBe('5.3 MB')
    expect(formatSize(3 * 1024 ** 3, 'en')).toBe('3 GB')
  })

  test('negative sizes clamp to zero', () => {
    expect(formatSize(-5, 'en')).toBe('0 B')
  })
})

describe('daysBetween', () => {
  test('counts calendar days, not 24h spans', () => {
    const late = new Date(2024, 2, 11, 23, 30).getTime()
    const early = new Date(2024, 2, 12, 0, 15).getTime()
    expect(daysBetween(late, early)).toBe(1)
    expect(daysBetween(early, early)).toBe(0)
  })
})

describe('formatListDate', () => {
  const now = new Date(2024, 2, 14, 15, 0).getTime()

  test('time today, weekday this week, day this year, short date before', () => {
    expect(formatListDate(new Date(2024, 2, 14, 9, 5).getTime(), now, 'en-GB')).toBe('09:05')
    expect(formatListDate(new Date(2024, 2, 11, 9, 5).getTime(), now, 'en-GB')).toBe('Mon')
    expect(formatListDate(new Date(2024, 0, 3).getTime(), now, 'en-GB')).toBe('3 Jan')
    expect(formatListDate(new Date(2019, 2, 12).getTime(), now, 'en-GB')).toBe('12/03/2019')
  })
})
