import { describe, expect, test } from 'bun:test'

import { createMatcher, displayStroke, type KeyInput, parseShortcut } from './shortcuts.ts'

const key = (k: string, extra: Partial<KeyInput> = {}): KeyInput => ({
  key: k,
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  ...extra,
})

describe('parseShortcut', () => {
  test('modifiers, aliases and sequences', () => {
    expect(parseShortcut('mod+k')).toEqual([{ key: 'k', mod: true, alt: false, shift: false }])
    expect(parseShortcut('g i').map((stroke) => stroke.key)).toEqual(['g', 'i'])
    expect(parseShortcut('esc')[0]?.key).toBe('Escape')
    expect(parseShortcut('down')[0]?.key).toBe('ArrowDown')
    expect(parseShortcut('?')[0]?.key).toBe('?')
  })
})

describe('createMatcher', () => {
  const bindings = {
    next: ['j', 'down'],
    inbox: ['g i'],
    all: ['g a'],
    palette: ['mod+k'],
    help: ['?'],
  } as const

  test('single keys', () => {
    const matcher = createMatcher(bindings, false)
    expect(matcher.feed(key('j'), 0)).toBe('next')
    expect(matcher.feed(key('ArrowDown'), 1)).toBe('next')
    expect(matcher.feed(key('J', { shiftKey: true }), 2)).toBeUndefined()
  })

  test('shifted punctuation matches its character', () => {
    const matcher = createMatcher(bindings, false)
    expect(matcher.feed(key('?', { shiftKey: true }), 0)).toBe('help')
  })

  test('sequences within the timeout', () => {
    const matcher = createMatcher(bindings, false, 1000)
    expect(matcher.feed(key('g'), 0)).toBeUndefined()
    expect(matcher.feed(key('i'), 500)).toBe('inbox')
    expect(matcher.feed(key('g'), 600)).toBeUndefined()
    expect(matcher.feed(key('a'), 2000)).toBeUndefined()
  })

  test('a broken sequence still matches the key on its own', () => {
    const matcher = createMatcher(bindings, false)
    expect(matcher.feed(key('g'), 0)).toBeUndefined()
    expect(matcher.feed(key('j'), 10)).toBe('next')
  })

  test('modifier keys alone do not reset a sequence', () => {
    const matcher = createMatcher(bindings, false)
    matcher.feed(key('g'), 0)
    matcher.feed(key('Shift', { shiftKey: true }), 5)
    expect(matcher.feed(key('i'), 10)).toBe('inbox')
  })

  test('mod is meta on mac and ctrl elsewhere', () => {
    expect(createMatcher(bindings, true).feed(key('k', { metaKey: true }), 0)).toBe('palette')
    expect(createMatcher(bindings, true).feed(key('k', { ctrlKey: true }), 0)).toBeUndefined()
    expect(createMatcher(bindings, false).feed(key('k', { ctrlKey: true }), 0)).toBe('palette')
    expect(createMatcher(bindings, false).feed(key('k'), 0)).toBeUndefined()
  })

  test('unmodified bindings ignore modified keys', () => {
    expect(createMatcher(bindings, true).feed(key('j', { metaKey: true }), 0)).toBeUndefined()
  })
})

describe('displayStroke', () => {
  test('platform labels', () => {
    expect(displayStroke('mod+k', true)).toEqual(['⌘K'])
    expect(displayStroke('mod+k', false)).toEqual(['Ctrl+K'])
    expect(displayStroke('g i', false)).toEqual(['G', 'I'])
    expect(displayStroke('down', false)).toEqual(['↓'])
  })
})
