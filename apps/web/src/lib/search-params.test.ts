import { describe, expect, test } from 'bun:test'

import {
  parseArchivesSettingsSearch,
  parseMailSearch,
  parseSearchRouteSearch,
} from './search-params.ts'

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

describe('parseArchivesSettingsSearch', () => {
  test('keeps a known action only with an archive', () => {
    expect(parseArchivesSettingsSearch({ archive: ' a1 ', add: 'connect' })).toEqual({
      archive: 'a1',
      add: 'connect',
    })
    expect(parseArchivesSettingsSearch({ add: 'import' })).toEqual({})
    expect(parseArchivesSettingsSearch({ archive: 'a1', add: 'delete' })).toEqual({ archive: 'a1' })
  })
})
