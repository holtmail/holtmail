import { describe, expect, test } from 'bun:test'

import { linkify, previewOf, withLinkTarget } from './text.ts'

describe('linkify', () => {
  test('finds urls and addresses, trims trailing punctuation', () => {
    expect(linkify('See https://example.com/a?b=1. Or mail bob@example.org!')).toEqual([
      { kind: 'text', text: 'See ' },
      { kind: 'link', text: 'https://example.com/a?b=1', href: 'https://example.com/a?b=1' },
      { kind: 'text', text: '. Or mail ' },
      { kind: 'link', text: 'bob@example.org', href: 'mailto:bob@example.org' },
      { kind: 'text', text: '!' },
    ])
  })

  test('www links get https', () => {
    expect(linkify('www.example.com')).toEqual([
      { kind: 'link', text: 'www.example.com', href: 'https://www.example.com' },
    ])
  })

  test('keeps a closing paren that belongs to the url', () => {
    const parts = linkify('https://example.com/wiki/A_(b)')
    expect(parts[0]?.text).toBe('https://example.com/wiki/A_(b)')
  })

  test('plain text stays one part', () => {
    expect(linkify('no links here')).toEqual([{ kind: 'text', text: 'no links here' }])
  })
})

describe('previewOf', () => {
  test('flattens whitespace, drops quotes and truncates', () => {
    expect(previewOf('Hello\n\n> quoted\nworld')).toBe('Hello world')
    expect(previewOf('a'.repeat(10), 5)).toBe('aaaa…')
    expect(previewOf(null)).toBe('')
  })
})

describe('withLinkTarget', () => {
  test('adds a base target into head, or in front', () => {
    expect(withLinkTarget('<html><head><title>x</title></head></html>')).toBe(
      '<html><head><base target="_blank"><title>x</title></head></html>',
    )
    expect(withLinkTarget('<p>x</p>')).toBe('<base target="_blank"><p>x</p>')
    expect(withLinkTarget('<head><base href="/"></head>')).toBe('<head><base href="/"></head>')
  })
})
