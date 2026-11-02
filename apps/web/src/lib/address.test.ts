import { describe, expect, test } from 'bun:test'

import { formatAddress, quoteLines, replyMailto, replySubject, senderLabel } from './address.ts'

describe('addresses', () => {
  test('sender label prefers the name', () => {
    expect(senderLabel([{ name: 'Ada Example', email: 'ada@example.com' }], '?')).toBe(
      'Ada Example',
    )
    expect(senderLabel([{ name: ' ', email: 'ada@example.com' }], '?')).toBe('ada@example.com')
    expect(senderLabel([], '?')).toBe('?')
  })

  test('formatAddress', () => {
    expect(formatAddress({ name: 'Ada', email: 'ada@example.com' })).toBe('Ada <ada@example.com>')
    expect(formatAddress({ email: 'ada@example.com' })).toBe('ada@example.com')
  })

  test('reply subject adds Re: once', () => {
    expect(replySubject('Hello')).toBe('Re: Hello')
    expect(replySubject('RE: Hello')).toBe('RE: Hello')
    expect(replySubject(null)).toBe('Re:')
  })

  test('mailto targets Reply-To before From and carries the message id', () => {
    const url = replyMailto({
      from: [{ email: 'ada@example.com' }],
      reply_to: [{ email: 'list@example.org' }],
      subject: 'Plans & dates',
      rfc_message_id: 'abc@example.com',
    })
    expect(url).toBe(
      'mailto:list@example.org?subject=Re%3A%20Plans%20%26%20dates&In-Reply-To=%3Cabc%40example.com%3E&References=%3Cabc%40example.com%3E',
    )
    expect(replyMailto({ from: [], reply_to: [] })).toBeUndefined()
  })

  test('the reply carries the reference chain and quotes the first lines', () => {
    const url = replyMailto(
      {
        from: [{ email: 'bob@example.org' }],
        reply_to: [],
        subject: 'Re: Plans',
        rfc_message_id: 'b@example.org',
        references: ['a@example.com'],
        text: '\nSounds good.\n\nSee you then.\n',
      },
      'Bob wrote:',
    )
    const params = new URLSearchParams(url?.split('?')[1])
    expect(params.get('subject')).toBe('Re: Plans')
    expect(params.get('In-Reply-To')).toBe('<b@example.org>')
    expect(params.get('References')).toBe('<a@example.com> <b@example.org>')
    expect(params.get('body')).toBe('\r\n\r\nBob wrote:\r\n> Sounds good.\r\n>\r\n> See you then.')
  })

  test('quotes at most a few lines and marks the cut', () => {
    expect(quoteLines('one\r\ntwo\nthree', 2)).toEqual(['> one', '> two', '> …'])
    expect(quoteLines('one\n\n\n', 2)).toEqual(['> one'])
    expect(quoteLines(null)).toEqual([])
  })
})
