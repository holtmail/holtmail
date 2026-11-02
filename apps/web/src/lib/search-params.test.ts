import { describe, expect, test } from 'bun:test'

import { parseMailSearch, parseSearchRouteSearch } from './search-params.ts'

describe('parseMailSearch', () => {
  test('keeps a message id', () => {
    expect(parseMailSearch({ m: 'abc' })).toEqual({ m: 'abc' })
    expect(parseMailSearch({ m: 42 })).toEqual({ m: '42' })
  })

  test('drops empty or invalid values', () => {
    expect(parseMailSearch({})).toEqual({})
    expect(parseMailSearch({ m: '  ' })).toEqual({})
    expect(parseMailSearch({ m: ['a'] })).toEqual({})
    expect(parseMailSearch({ m: null })).toEqual({})
  })
})

describe('parseSearchRouteSearch', () => {
  test('q defaults to empty', () => {
    expect(parseSearchRouteSearch({})).toEqual({ q: '' })
    expect(parseSearchRouteSearch({ q: 'from:ada', m: 'x' })).toEqual({ q: 'from:ada', m: 'x' })
  })
})
