import { describe, expect, test } from 'bun:test'

import { dayGroup, groupByDay } from './group.ts'

const now = new Date(2024, 2, 14, 15, 0).getTime()
const at = (...parts: [number, number, number, number?]) =>
  new Date(parts[0], parts[1], parts[2], parts[3] ?? 12).getTime()

describe('dayGroup', () => {
  test('labels', () => {
    expect(dayGroup(at(2024, 2, 14, 1), now, 'en-GB').label).toBe('Today')
    expect(dayGroup(at(2024, 2, 13), now, 'en-GB').label).toBe('Yesterday')
    expect(dayGroup(at(2024, 2, 11), now, 'en-GB').label).toBe('Monday')
    expect(dayGroup(at(2024, 2, 7), now, 'en-GB').label).toBe('7 March')
    expect(dayGroup(at(2019, 2, 12), now, 'en-GB').label).toBe('March 2019')
  })

  test('future dates count as today', () => {
    expect(dayGroup(at(2024, 5, 1), now, 'en-GB').label).toBe('Today')
  })

  test('older years group by month', () => {
    expect(dayGroup(at(2019, 2, 12), now).key).toBe(dayGroup(at(2019, 2, 28), now).key)
    expect(dayGroup(at(2024, 1, 12), now).key).not.toBe(dayGroup(at(2024, 1, 13), now).key)
  })
})

describe('groupByDay', () => {
  test('inserts one header before each group, keeps item order and indexes', () => {
    const items = [
      { id: 'a', date: at(2024, 2, 14, 10) },
      { id: 'b', date: at(2024, 2, 14, 8) },
      { id: 'c', date: at(2024, 2, 13) },
      { id: 'd', date: at(2018, 4, 2) },
      { id: 'e', date: at(2018, 4, 1) },
    ]
    const rows = groupByDay(items, now, 'en-GB')
    expect(rows.map((row) => (row.kind === 'header' ? row.label : row.item.id))).toEqual([
      'Today',
      'a',
      'b',
      'Yesterday',
      'c',
      'May 2018',
      'd',
      'e',
    ])
    expect(rows.flatMap((row) => (row.kind === 'item' ? [row.index] : []))).toEqual([0, 1, 2, 3, 4])
    expect(new Set(rows.map((row) => row.key)).size).toBe(rows.length)
  })

  test('empty input gives no rows', () => {
    expect(groupByDay([], now)).toEqual([])
  })
})
