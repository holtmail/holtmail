import { describe, expect, test } from 'bun:test'

import { effectivePort, imapMoves, jmapMoves } from './connection.ts'

const saved = { host: 'imap.example.com', port: 993, security: 'tls' }

describe('imapMoves', () => {
  test('keeps the password on the same host and port', () => {
    expect(imapMoves(saved, { ...saved, host: ' IMAP.example.com ' })).toBe(false)
    expect(imapMoves(saved, { ...saved, security: 'starttls' })).toBe(false)
  })

  test('needs the password for another host, port or plain text', () => {
    expect(imapMoves(saved, { ...saved, host: 'imap.example.net' })).toBe(true)
    expect(imapMoves(saved, { ...saved, port: 1993 })).toBe(true)
    expect(imapMoves(saved, { ...saved, security: 'plain' })).toBe(true)
    expect(imapMoves({ ...saved, security: 'plain' }, { ...saved, security: 'starttls' })).toBe(
      false,
    )
  })

  test('counts an empty port as the default of the security', () => {
    expect(effectivePort('', 'tls')).toBe(993)
    expect(effectivePort('', 'starttls')).toBe(143)
    expect(effectivePort('1143', 'starttls')).toBe(1143)
    expect(imapMoves(saved, { ...saved, port: effectivePort('', 'starttls') })).toBe(true)
  })
})

describe('jmapMoves', () => {
  const session = 'https://api.example.com/jmap/session'

  test('keeps the secret on the same origin', () => {
    expect(jmapMoves(session, session)).toBe(false)
    expect(jmapMoves(session, ' api.example.com ')).toBe(false)
    expect(jmapMoves(session, 'https://API.example.com:443/other')).toBe(false)
  })

  test('needs the secret for another scheme, host or port', () => {
    expect(jmapMoves(session, 'https://jmap.example.org/jmap/session')).toBe(true)
    expect(jmapMoves(session, 'https://api.example.com:8443/jmap')).toBe(true)
    expect(jmapMoves(session, 'http://api.example.com/jmap')).toBe(true)
    expect(jmapMoves(session, '')).toBe(true)
    expect(jmapMoves(session, 'https://')).toBe(true)
  })
})
