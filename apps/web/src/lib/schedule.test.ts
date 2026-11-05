import { describe, expect, test } from 'bun:test'

import {
  formatInterval,
  formatMinAge,
  formatNextSync,
  intervalChoices,
  minAgeOptions,
  parseExcluded,
} from './schedule.ts'

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

test('minimum age choices', () => {
  expect(minAgeOptions(null)).toEqual([7, 30, 90, 365])
  expect(minAgeOptions(30)).toEqual([7, 30, 90, 365])
  expect(minAgeOptions(14)).toEqual([7, 14, 30, 90, 365])
  expect(minAgeOptions(null).map(formatMinAge)).toEqual([
    'Mail older than a week',
    'Mail older than 30 days',
    'Mail older than 90 days',
    'Mail older than a year',
  ])
  expect(formatMinAge(null)).toBe('All mail, as it arrives')
  expect(formatMinAge(0)).toBe('All mail, as it arrives')
  expect(formatMinAge(1)).toBe('Mail older than a day')
})
