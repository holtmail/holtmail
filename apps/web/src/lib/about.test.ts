import { describe, expect, test } from 'bun:test'

import type { DataFile, DataUpdates } from '@holtmail/api-client'

import { strings } from '../strings.ts'
import { attributions, dataFileRows, dataName, updateLines, ut1Loaded } from './about.ts'
import { formatFullDate } from './format.ts'

const wikidata: DataFile = {
  name: 'wikidata',
  file: 'wikidata-categories.tsv.gz',
  version: '2026.11.01',
  rows: 123456,
  license: 'CC0-1.0',
  origin: 'bundled',
  attribution: null,
}

const ut1: DataFile = {
  name: 'ut1',
  file: 'ut1-categories.tsv.gz',
  version: '2026.11.02',
  rows: 9876,
  license: 'CC-BY-SA-4.0',
  origin: 'downloaded',
  attribution: 'Contains data from UT1 (Université Toulouse Capitole), CC BY-SA 4.0',
}

const off: DataUpdates = { enabled: false, data_sources: ['wikidata'] }

describe('dataName', () => {
  test('names known tables and keeps unknown ones', () => {
    expect(dataName('wikidata')).toBe('Wikidata')
    expect(dataName('ut1')).toBe('UT1')
    expect(dataName('other')).toBe('other')
  })
})

describe('dataFileRows', () => {
  test('formats rows and origin per file in order', () => {
    expect(dataFileRows([wikidata, ut1], 'en')).toEqual([
      {
        key: 'wikidata:wikidata-categories.tsv.gz',
        name: 'Wikidata',
        file: 'wikidata-categories.tsv.gz',
        version: '2026.11.01',
        rows: '123,456 domains',
        license: 'CC0-1.0',
        origin: 'Bundled with Holtmail',
      },
      {
        key: 'ut1:ut1-categories.tsv.gz',
        name: 'UT1',
        file: 'ut1-categories.tsv.gz',
        version: '2026.11.02',
        rows: '9,876 domains',
        license: 'CC-BY-SA-4.0',
        origin: 'Downloaded',
      },
    ])
  })

  test('is empty without files', () => {
    expect(dataFileRows([])).toEqual([])
  })
})

describe('ut1Loaded and attributions', () => {
  test('nothing for wikidata alone', () => {
    expect(ut1Loaded([wikidata])).toBe(false)
    expect(attributions([wikidata])).toEqual([])
  })

  test('the server text when UT1 is loaded', () => {
    expect(ut1Loaded([wikidata, ut1])).toBe(true)
    expect(attributions([wikidata, ut1])).toEqual([strings.about.ut1Attribution])
  })

  test('the UT1 line even when the server sends none', () => {
    expect(attributions([{ ...ut1, attribution: null }])).toEqual([strings.about.ut1Attribution])
    expect(attributions([{ ...ut1, attribution: '  ' }])).toEqual([strings.about.ut1Attribution])
  })

  test('each line once and other tables keep their own text', () => {
    const custom = { ...wikidata, attribution: 'Data from example.org' }
    expect(attributions([custom, ut1, { ...ut1, file: 'ut1-b.tsv.gz' }])).toEqual([
      'Data from example.org',
      strings.about.ut1Attribution,
    ])
  })
})

describe('updateLines', () => {
  test('off', () => {
    expect(updateLines(off)).toEqual([strings.about.updatesOff])
  })

  test('on and never checked', () => {
    expect(updateLines({ enabled: true, data_sources: ['wikidata', 'ut1'] })).toEqual([
      'Updates are on for Wikidata, UT1, checked weekly.',
      strings.about.neverChecked,
    ])
  })

  test('on with every outcome', () => {
    const at = Date.UTC(2026, 10, 5, 9, 30)
    const date = formatFullDate(at, 'en')
    for (const [outcome, label] of Object.entries(strings.about.outcomes)) {
      const lines = updateLines(
        {
          enabled: true,
          data_sources: ['wikidata'],
          last_checked_at: at,
          last_outcome: outcome as DataUpdates['last_outcome'],
        },
        'en',
      )
      expect(lines).toEqual([
        'Updates are on for Wikidata, checked weekly.',
        `Last checked ${date}: ${label}.`,
      ])
    }
  })

  test('a check without an outcome shows only the date', () => {
    const at = Date.UTC(2026, 10, 5, 9, 30)
    expect(
      updateLines(
        { enabled: true, data_sources: ['wikidata'], last_checked_at: at, last_outcome: null },
        'en',
      )[1],
    ).toBe(`Last checked ${formatFullDate(at, 'en')}.`)
  })
})
