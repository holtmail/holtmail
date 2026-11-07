import { describe, expect, test } from 'bun:test'

import { strings } from '../strings.ts'
import { stillReading } from './reading.ts'

describe('stillReading', () => {
  test('nothing without a status', () => {
    expect(stillReading(undefined)).toBeNull()
  })

  test('nothing when caught up', () => {
    expect(stillReading({ pending: 0, reindex: 0, categorize: 0, running: true })).toBeNull()
  })

  test('adds pending, categorize and reindex', () => {
    expect(stillReading({ pending: 3, reindex: 0, categorize: 0, running: true })).toBe(3)
    expect(stillReading({ pending: 0, reindex: 2, categorize: 0, running: false })).toBe(2)
    expect(stillReading({ pending: 0, reindex: 0, categorize: 5, running: false })).toBe(5)
    expect(stillReading({ pending: 1200, reindex: 30, categorize: 4, running: true })).toBe(1234)
  })

  test('message names the count', () => {
    expect(strings.insights.stillReading('1,234')).toBe(
      'Some messages are still being read (1,234 left)',
    )
  })
})
