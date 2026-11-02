import { describe, expect, test } from 'bun:test'

import { insertToken } from './query.ts'

describe('insertToken', () => {
  test('into an empty field', () => {
    expect(insertToken('', 0, 0, 'from:')).toEqual({ value: 'from:', caret: 5 })
    expect(insertToken('', 0, 0, 'has:attachment')).toEqual({ value: 'has:attachment ', caret: 15 })
  })

  test('after a word adds a space before', () => {
    expect(insertToken('report', 6, 6, 'from:')).toEqual({ value: 'report from:', caret: 12 })
  })

  test('replaces a selection and keeps the rest', () => {
    expect(insertToken('a XX b', 2, 4, 'is:unread')).toEqual({ value: 'a is:unread b', caret: 11 })
  })

  test('clamps out-of-range positions', () => {
    expect(insertToken('a', 9, 9, 'to:').value).toBe('a to:')
  })
})
