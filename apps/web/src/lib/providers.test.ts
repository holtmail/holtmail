import { describe, expect, test } from 'bun:test'

import { defaultPort, domainOf, guessServer, providerFor } from './providers.ts'

describe('providerFor', () => {
  test('knows big providers by domain, ignoring case and spaces', () => {
    expect(providerFor('ada@gmail.com')?.id).toBe('gmail')
    expect(providerFor(' Ada@GoogleMail.com ')?.id).toBe('gmail')
    expect(providerFor('ada@yandex.ru')?.id).toBe('yandex')
    expect(providerFor('ada@me.com')?.id).toBe('icloud')
  })

  test('unknown or incomplete addresses have no provider', () => {
    expect(providerFor('ada@example.com')).toBeUndefined()
    expect(providerFor('ada')).toBeUndefined()
    expect(providerFor('')).toBeUndefined()
  })
})

describe('guessServer', () => {
  test('uses the preset of a known provider', () => {
    expect(guessServer('bob@proton.me')).toEqual({
      host: '127.0.0.1',
      port: 1143,
      security: 'starttls',
    })
  })

  test('guesses imap.<domain> for other domains', () => {
    expect(guessServer('ada@Example.org')).toEqual({
      host: 'imap.example.org',
      port: 993,
      security: 'tls',
    })
    expect(guessServer('ada@localhost')).toBeUndefined()
    expect(guessServer('ada')).toBeUndefined()
  })

  test('domainOf takes the part after the last @', () => {
    expect(domainOf('"a@b"@example.com')).toBe('example.com')
  })

  test('default ports follow the security', () => {
    expect(defaultPort('tls')).toBe(993)
    expect(defaultPort('starttls')).toBe(143)
    expect(defaultPort('plain')).toBe(143)
  })
})
