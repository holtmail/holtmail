import { describe, expect, test } from 'bun:test'

import {
  parseArchivesSettingsSearch,
  parseMailSearch,
  parseCalendarSearch,
  parsePeopleSearch,
  parseSearchRouteSearch,
} from './search-params.ts'

describe('parsePeopleSearch', () => {
  test('keeps the person, the text and the filters', () => {
    expect(parsePeopleSearch({ p: 'p1', q: 'ada', source: 'mail', diff: true })).toEqual({
      p: 'p1',
      q: 'ada',
      source: 'mail',
      diff: true,
    })
    expect(parsePeopleSearch({ diff: 'true' })).toEqual({ diff: true })
  })

  test('drops empty or invalid values', () => {
    expect(parsePeopleSearch({ p: ' ', q: '  ', source: '', diff: 'no' })).toEqual({})
    expect(parsePeopleSearch({ p: ['x'], diff: false })).toEqual({})
  })
})

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

describe('parseCalendarSearch', () => {
  test('keeps a known view, a real date, hidden calendars and the removed toggle', () => {
    expect(
      parseCalendarSearch({ view: 'week', d: '2025-03-30', off: ' a, ,b ', removed: 'true' }),
    ).toEqual({ view: 'week', d: '2025-03-30', off: 'a,b', removed: true })
    expect(parseCalendarSearch({ view: 'year', d: '2025-02-30', off: '' })).toEqual({})
  })
})
