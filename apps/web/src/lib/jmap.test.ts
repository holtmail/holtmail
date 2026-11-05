import { describe, expect, test } from 'bun:test'

import { fastmailSession, type JmapForm, jmapMissing, jmapRequest } from './jmap.ts'

const form: JmapForm = {
  preset: 'other',
  url: ' jmap.example.com ',
  auth: 'password',
  username: ' ada@example.com ',
  secret: 'correct horse',
  name: '',
}

describe('jmapRequest', () => {
  test('signs in to Fastmail with a token at its session address', () => {
    expect(jmapRequest({ ...form, preset: 'fastmail', secret: ' fmu1-token ' })).toEqual({
      url: fastmailSession,
      auth: 'token',
      username: '',
      password: 'fmu1-token',
      name: 'Fastmail',
    })
  })

  test('keeps the user name only for a password', () => {
    expect(jmapRequest(form)).toEqual({
      url: 'jmap.example.com',
      auth: 'password',
      username: 'ada@example.com',
      password: 'correct horse',
      name: null,
    })
    expect(jmapRequest({ ...form, auth: 'token' }).username).toBe('')
  })
})

describe('jmapMissing', () => {
  test('names the first empty field', () => {
    expect(jmapMissing({ ...form, url: ' ' })).toBe('url')
    expect(jmapMissing({ ...form, username: '' })).toBe('username')
    expect(jmapMissing({ ...form, auth: 'token', username: '' })).toBe(undefined)
    expect(jmapMissing({ ...form, preset: 'fastmail', url: '', secret: '' })).toBe('secret')
    expect(jmapMissing(form)).toBe(undefined)
  })
})
