import { describe, expect, test } from 'bun:test'

import { formatInterval, formatNextSync, intervalChoices, parseExcluded } from './schedule.ts'

describe('formatInterval', () => {
  test('names every choice', () => {
    expect(intervalChoices.map(formatInterval)).toEqual([
      'Every 5 minutes',
      'Every 15 minutes',
      'Every 30 minutes',
      'Every hour',
      'Every 3 hours',
      'Every 6 hours',
      'Every 12 hours',
      'Once a day',
      'Only when you press Sync now',
    ])
    expect(formatInterval(90)).toBe('Every 90 minutes')
    expect(formatInterval(2880)).toBe('Every 2 days')
  })
})

test('formatNextSync', () => {
  expect(formatNextSync(30_000, 0, 'en')).toBe('Next sync in under a minute')
  expect(formatNextSync(12 * 60_000, 0, 'en')).toBe('Next sync in 12 minutes')
  expect(formatNextSync(3 * 3_600_000, 0, 'en')).toBe('Next sync in 3 hours')
})

test('parseExcluded', () => {
  expect(parseExcluded('Junk, Trash\n\n[Gmail]/All Mail , Junk')).toEqual([
    'Junk',
    'Trash',
    '[Gmail]/All Mail',
  ])
})
