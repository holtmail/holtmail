import { describe, expect, test } from 'bun:test'

import type { Label } from '@holtmail/api-client'

import { isSystemLabel, listChips, orderLabels, readerLabels } from './labels.ts'

const label = (name: string, messages = 1): Label => ({ name, messages, unread: 0 })

describe('orderLabels', () => {
  test('system labels first, then others by name, nested under their parents', () => {
    const ordered = orderLabels([
      label('Work/Projects/2025'),
      label('Sent'),
      label('receipts 10'),
      label('Inbox'),
      label('Work'),
      label('receipts 9'),
      label('Travel/Rome'),
    ])
    expect(ordered.map((node) => [node.label.name, node.name, node.depth])).toEqual([
      ['Inbox', 'Inbox', 0],
      ['Sent', 'Sent', 0],
      ['receipts 9', 'receipts 9', 0],
      ['receipts 10', 'receipts 10', 0],
      ['Travel/Rome', 'Travel/Rome', 0],
      ['Work', 'Work', 0],
      ['Work/Projects/2025', 'Projects/2025', 1],
    ])
  })

  test('is empty without labels', () => {
    expect(orderLabels([])).toEqual([])
  })
})

describe('listChips', () => {
  test('hides system labels and the label shown, and counts the rest', () => {
    expect(listChips(['Inbox', 'Important', 'Work', 'Travel', 'Receipts'], 'Travel')).toEqual({
      shown: ['Work', 'Receipts'],
      more: 0,
    })
    expect(listChips(['A', 'B', 'C', 'D'])).toEqual({ shown: ['A', 'B'], more: 2 })
    expect(listChips([])).toEqual({ shown: [], more: 0 })
  })
})

describe('readerLabels', () => {
  test('system labels first, then the rest by name', () => {
    expect(readerLabels(['work', 'Inbox', 'Archive 2', 'Starred'])).toEqual([
      'Inbox',
      'Starred',
      'Archive 2',
      'work',
    ])
  })
})

describe('isSystemLabel', () => {
  test('knows Gmail system labels', () => {
    expect(isSystemLabel('Inbox')).toBe(true)
    expect(isSystemLabel('inbox')).toBe(false)
    expect(isSystemLabel('Work')).toBe(false)
  })
})
