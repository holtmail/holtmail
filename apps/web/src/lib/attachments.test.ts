import { describe, expect, test } from 'bun:test'

import { attachmentKind } from './attachments.ts'

describe('attachmentKind', () => {
  test('by content type', () => {
    expect(attachmentKind('image/png')).toBe('image')
    expect(attachmentKind('application/pdf')).toBe('pdf')
    expect(attachmentKind('text/calendar; method=REQUEST')).toBe('calendar')
    expect(attachmentKind('application/zip')).toBe('archive')
  })

  test('by extension when the type is generic', () => {
    expect(attachmentKind('application/octet-stream', 'Budget.XLSX')).toBe('spreadsheet')
    expect(attachmentKind('application/octet-stream', 'notes')).toBe('other')
  })
})
