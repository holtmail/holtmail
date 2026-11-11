import { describe, expect, test } from 'bun:test'

import { diffLines, diffText, exportFileName, exportText, importText } from './rules-json.ts'
import type { FlowDefinition } from './rules.ts'

const flow: FlowDefinition = {
  format: 1,
  trigger: { type: 'message.archived', next: 'l', position: { x: 0, y: 0 } },
  nodes: [{ id: 'l', type: 'label', params: { name: 'Orders' }, position: { x: 288, y: 0 } }],
}

describe('import and export', () => {
  test('an export imports back to the same flow', () => {
    const text = exportText(flow)
    expect(text.endsWith('\n')).toBe(true)
    expect(importText(text)).toEqual({ flow })
  })

  test('import names what is wrong', () => {
    expect(importText('{nope')).toEqual({ error: 'json' })
    expect(importText('{"nodes": []}')).toEqual({ error: 'flow' })
  })

  test('file names', () => {
    expect(exportFileName('Orders from the shop')).toBe('orders-from-the-shop.json')
    expect(exportFileName('Заказы: магазин')).toBe('заказы-магазин.json')
    expect(exportFileName('  !!  ')).toBe('flow.json')
  })
})

describe('diff', () => {
  test('positions are not part of the compared text', () => {
    const moved = { ...flow, trigger: { ...flow.trigger, position: { x: 32, y: 64 } } }
    expect(diffText(moved)).toEqual(diffText(flow))
    expect(diffText(null)).toEqual([])
  })

  test('changed lines with context and folded gaps', () => {
    const before = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i']
    const after = ['a', 'b', 'c', 'd', 'E', 'f', 'g', 'h', 'i', 'j']
    expect(diffLines(before, after, 1)).toEqual([
      { kind: 'gap', count: 3 },
      { kind: 'same', text: 'd' },
      { kind: 'removed', text: 'e' },
      { kind: 'added', text: 'E' },
      { kind: 'same', text: 'f' },
      { kind: 'gap', count: 2 },
      { kind: 'same', text: 'i' },
      { kind: 'added', text: 'j' },
    ])
  })

  test('equal inputs fold to one gap, empty inputs to nothing', () => {
    expect(diffLines(['a', 'b'], ['a', 'b'])).toEqual([{ kind: 'gap', count: 2 }])
    expect(diffLines([], [])).toEqual([])
    expect(diffLines([], ['a'])).toEqual([{ kind: 'added', text: 'a' }])
  })
})
