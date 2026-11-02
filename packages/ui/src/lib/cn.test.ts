import { describe, expect, test } from 'bun:test'

import { cn } from './cn.ts'

describe('cn', () => {
  test('joins conditional classes', () => {
    expect(cn('px-2', false, null, undefined, { 'text-fg': true, hidden: false })).toBe(
      'px-2 text-fg',
    )
  })

  test('later token colors win over earlier ones', () => {
    expect(cn('bg-bg text-fg', 'bg-accent text-accent-fg')).toBe('bg-accent text-accent-fg')
  })

  test('keeps font size and color apart', () => {
    expect(cn('text-sm text-fg-muted', 'text-xs')).toBe('text-fg-muted text-xs')
  })

  test('merges token durations and easings', () => {
    expect(cn('duration-fast-01 ease-productive', 'duration-moderate-02 ease-expressive')).toBe(
      'duration-moderate-02 ease-expressive',
    )
  })

  test('merges layout tokens', () => {
    expect(cn('w-pane-folders h-topbar', 'w-pane-list h-8')).toBe('w-pane-list h-8')
  })
})
