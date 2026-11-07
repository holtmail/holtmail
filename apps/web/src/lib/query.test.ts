import { describe, expect, test } from 'bun:test'

import { insertToken, operators } from './query.ts'

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

describe('operators', () => {
  test('lists insight operators once', () => {
    for (const token of [
      'category:',
      'has:code',
      'has:link',
      'has:unsubscribe',
      'has:reservation',
      'has:order',
      'has:invoice',
      'has:parcel',
      'has:pass',
      'is:newsletter',
      'is:notification',
      'is:personal',
      'is:list',
    ]) {
      expect(operators.filter((operator) => operator === token)).toHaveLength(1)
    }
    expect(new Set(operators).size).toBe(operators.length)
  })

  test('insertToken handles the new tokens', () => {
    expect(insertToken('trip', 4, 4, 'category:')).toEqual({ value: 'trip category:', caret: 14 })
    expect(insertToken('', 0, 0, 'has:code')).toEqual({ value: 'has:code ', caret: 9 })
    expect(insertToken('a b', 1, 1, 'is:newsletter')).toEqual({
      value: 'a is:newsletter b',
      caret: 15,
    })
  })
})
