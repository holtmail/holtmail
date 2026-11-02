import { describe, expect, test } from 'bun:test'

import { isFlagged, isUnread } from './flags.ts'

describe('flags', () => {
  test('unread means no \\Seen, in any letter case', () => {
    expect(isUnread([])).toBe(true)
    expect(isUnread(['\\Seen'])).toBe(false)
    expect(isUnread(['\\SEEN', '$label1'])).toBe(false)
    expect(isUnread(['$seen'])).toBe(true)
  })

  test('flagged means \\Flagged', () => {
    expect(isFlagged(['\\Flagged'])).toBe(true)
    expect(isFlagged(['\\flagged'])).toBe(true)
    expect(isFlagged(['$Flagged'])).toBe(false)
  })
})
