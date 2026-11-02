import { describe, expect, test } from 'bun:test'

import { parseTheme } from './theme.ts'

describe('parseTheme', () => {
  test('keeps known themes', () => {
    expect(parseTheme('light')).toBe('light')
    expect(parseTheme('dark')).toBe('dark')
    expect(parseTheme('system')).toBe('system')
  })

  test('falls back to system', () => {
    expect(parseTheme(null)).toBe('system')
    expect(parseTheme(undefined)).toBe('system')
    expect(parseTheme('sepia')).toBe('system')
  })
})
