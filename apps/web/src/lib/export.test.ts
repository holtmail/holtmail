import { describe, expect, test } from 'bun:test'

import { exportHref, exportScope } from './export.ts'

describe('export', () => {
  test('narrows to what the list shows', () => {
    expect(exportScope({ kind: 'all' })).toEqual({})
    expect(exportScope({ kind: 'folder', folderId: 'f1' })).toEqual({ folder: 'f1' })
    expect(exportScope({ kind: 'label', label: 'Travel' })).toEqual({ label: 'Travel' })
    expect(exportScope({ kind: 'search', q: 'from:ada' })).toEqual({ q: 'from:ada' })
  })

  test('has nothing to export for an empty search', () => {
    expect(exportScope({ kind: 'search', q: '  ' })).toBeNull()
    expect(exportHref('a1', { kind: 'search', q: '' }, 'zip')).toBeNull()
  })

  test('builds the download address', () => {
    expect(exportHref('a1', { kind: 'all' }, 'mbox')).toBe('/api/archives/a1/export?format=mbox')
    expect(exportHref('a1', { kind: 'label', label: 'News & updates' }, 'zip')).toBe(
      '/api/archives/a1/export?format=zip&label=News+%26+updates',
    )
    expect(exportHref('a 1', { kind: 'search', q: 'is:unread' }, 'mbox')).toBe(
      '/api/archives/a%201/export?format=mbox&q=is%3Aunread',
    )
  })
})
